use bytes::Bytes;
use gateway_core::{GatewayError, GatewayResult};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const DEFAULT_MAX_BUFFERED_REQUESTS: usize = 8;
pub const DEFAULT_MAX_INFLIGHT_BUFFER_BYTES: usize = 512 * 1024 * 1024;
pub const SMALL_BODY_MAX_BYTES: usize = 256 * 1024;
const REQUEST_WORKING_OVERHEAD: usize = 64 * 1024;

/// Per-process bounds on uploads waiting before admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyWaitPolicy {
    pub max_waiters: usize,
    pub timeout: Duration,
}

impl Default for BodyWaitPolicy {
    fn default() -> Self {
        Self {
            max_waiters: 0,
            timeout: Duration::from_secs(10),
        }
    }
}

impl BodyWaitPolicy {
    pub fn validate(self, max_slots: usize) -> GatewayResult<()> {
        if self.max_waiters > Semaphore::MAX_PERMITS
            || (self.max_waiters > 0 && max_slots > Semaphore::MAX_PERMITS)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(600)
        {
            return Err(GatewayError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct BodyWaitQueue {
    slots: Arc<Semaphore>,
    large_slots: Option<Arc<Semaphore>>,
    waiters: Semaphore,
    policy: BodyWaitPolicy,
}

/// A header reservation becomes part of the buffered body lease on its first chunk.
#[derive(Debug)]
pub(crate) struct UploadSlot {
    header_reservation: Option<gateway_telemetry::BodyGaugeGuard>,
    _total: OwnedSemaphorePermit,
    large: Option<OwnedSemaphorePermit>,
}

impl BodyWaitQueue {
    fn large_slots(&self, expected: Option<usize>) -> Option<&Arc<Semaphore>> {
        self.large_slots
            .as_ref()
            .filter(|_| expected.is_none_or(|length| length > SMALL_BODY_MAX_BYTES))
    }

    pub(crate) fn try_slot(&self, expected: Option<usize>) -> Option<UploadSlot> {
        let large = match self.large_slots(expected) {
            Some(slots) => Some(slots.clone().try_acquire_owned().ok()?),
            None => None,
        };
        Some(UploadSlot {
            _total: self.slots.clone().try_acquire_owned().ok()?,
            header_reservation: Some(gateway_telemetry::body_header_slot_reserved()),
            large,
        })
    }

    pub(crate) async fn wait_for_slot(&self, expected: Option<usize>) -> GatewayResult<UploadSlot> {
        let _place = self
            .waiters
            .try_acquire()
            .map_err(|_| queue_rejection("queue_full"))?;
        let _waiting = gateway_telemetry::body_waiter_started();
        // Acquire shared-class capacity first so that waiting large uploads cannot
        // occupy the count reserved for small bodies. One deadline covers both waits.
        tokio::time::timeout(self.policy.timeout, async {
            let large = match self.large_slots(expected) {
                Some(slots) => Some(slots.clone().acquire_owned().await?),
                None => None,
            };
            Ok::<_, tokio::sync::AcquireError>(UploadSlot {
                _total: self.slots.clone().acquire_owned().await?,
                header_reservation: Some(gateway_telemetry::body_header_slot_reserved()),
                large,
            })
        })
        .await
        .map_err(|_| queue_rejection("queue_timeout"))?
        .map_err(|_| GatewayError::GatewayOverloaded)
    }
}

fn queue_rejection(reason: &'static str) -> GatewayError {
    gateway_telemetry::record_body_admission_rejection(reason);
    GatewayError::GatewayOverloaded
}

/// Optional, additive working-set admission. This is a conservative estimate,
/// not an allocator/RSS limit: JSON shape and third-party guardrails still matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeAwareBodyPolicy {
    pub max_working_bytes: usize,
    /// Opt-in ceiling for elastic admission. Memory charges determine useful
    /// concurrency; this larger count is a backstop for per-request state.
    pub elastic_max_requests: Option<usize>,
    pub reserved_small_requests: usize,
    pub workspace_multiplier: usize,
    pub upload_timeout: std::time::Duration,
}

impl SizeAwareBodyPolicy {
    pub fn validate(self, max_requests: usize) -> GatewayResult<()> {
        if self.max_working_bytes == 0
            || self
                .elastic_max_requests
                .is_some_and(|limit| limit < max_requests)
            || self.reserved_small_requests >= max_requests
            || !(1..=16).contains(&self.workspace_multiplier)
            || self.upload_timeout.is_zero()
            || self.upload_timeout > std::time::Duration::from_secs(600)
            || self
                .small_reserve()
                .is_none_or(|bytes| bytes >= self.max_working_bytes)
        {
            return Err(GatewayError::InvalidConfiguration);
        }
        Ok(())
    }

    fn small_reserve(self) -> Option<usize> {
        SMALL_BODY_MAX_BYTES
            .checked_mul(1 + self.workspace_multiplier)?
            .checked_add(REQUEST_WORKING_OVERHEAD)?
            .checked_mul(self.reserved_small_requests)
    }
}

#[derive(Debug, Default)]
struct WorkingSet {
    bytes: usize,
    large_bytes: usize,
    large_requests: usize,
}

#[derive(Debug)]
struct BodyAdmissionState {
    configured_max_requests: usize,
    max_requests: usize,
    max_bytes: usize,
    active_requests: AtomicUsize,
    active_bytes: AtomicUsize,
    size_policy: Option<SizeAwareBodyPolicy>,
    working: Mutex<WorkingSet>,
    wait_queue: Option<BodyWaitQueue>,
}

#[derive(Debug, Clone)]
pub struct BodyAdmissionController {
    state: Arc<BodyAdmissionState>,
}

#[derive(Debug)]
pub struct BodyAdmissionLease {
    state: Arc<BodyAdmissionState>,
    slot: Option<UploadSlot>,
    reserved_bytes: usize,
    admitted_at: Instant,
    expected_length: Option<usize>,
    allocation_capacity: usize,
    rewrite_capacity: usize,
    working_bytes: usize,
    extent: usize,
    processing: bool,
    compact_workspace: bool,
    large: bool,
}

struct AdmittedBody {
    // Drop the buffer before returning its capacity to concurrent requests.
    body: Bytes,
    _lease: BodyAdmissionLease,
}

impl AsRef<[u8]> for AdmittedBody {
    fn as_ref(&self) -> &[u8] {
        self.body.as_ref()
    }
}

impl BodyAdmissionController {
    pub fn new(max_requests: usize, max_bytes: usize) -> GatewayResult<Self> {
        Self::with_size_policy(max_requests, max_bytes, None)
    }

    pub fn with_size_policy(
        max_requests: usize,
        max_bytes: usize,
        size_policy: Option<SizeAwareBodyPolicy>,
    ) -> GatewayResult<Self> {
        Self::with_policies(
            max_requests,
            max_bytes,
            size_policy,
            BodyWaitPolicy::default(),
        )
    }

    pub(crate) fn with_policies(
        max_requests: usize,
        max_bytes: usize,
        size_policy: Option<SizeAwareBodyPolicy>,
        wait: BodyWaitPolicy,
    ) -> GatewayResult<Self> {
        if max_requests == 0 || max_bytes == 0 {
            return Err(GatewayError::InvalidConfiguration);
        }
        if let Some(policy) = size_policy {
            policy.validate(max_requests)?;
        }
        let configured_max_requests = max_requests;
        let max_requests = size_policy
            .and_then(|policy| policy.elastic_max_requests)
            .unwrap_or(max_requests);
        wait.validate(max_requests)?;
        let wait_queue = (wait.max_waiters > 0).then(|| BodyWaitQueue {
            slots: Arc::new(Semaphore::new(max_requests)),
            large_slots: size_policy
                .filter(|p| p.reserved_small_requests > 0)
                .map(|p| Arc::new(Semaphore::new(max_requests - p.reserved_small_requests))),
            waiters: Semaphore::new(wait.max_waiters),
            policy: wait,
        });
        gateway_telemetry::runtime::configure_body_capacity(max_requests, max_bytes);
        Ok(Self {
            state: Arc::new(BodyAdmissionState {
                configured_max_requests,
                max_requests,
                max_bytes,
                active_requests: AtomicUsize::new(0),
                active_bytes: AtomicUsize::new(0),
                size_policy,
                working: Mutex::new(WorkingSet::default()),
                wait_queue,
            }),
        })
    }

    pub fn try_acquire(&self) -> GatewayResult<BodyAdmissionLease> {
        self.try_acquire_sized(None)
    }

    pub fn try_acquire_sized(
        &self,
        expected_length: Option<usize>,
    ) -> GatewayResult<BodyAdmissionLease> {
        self.acquire_with_slot(expected_length, None)
    }

    pub(crate) fn wait_queue(&self) -> Option<&BodyWaitQueue> {
        self.state.wait_queue.as_ref()
    }

    pub(crate) fn wait_policy(&self) -> BodyWaitPolicy {
        self.wait_queue().map(|q| q.policy).unwrap_or_default()
    }

    pub(crate) fn acquire_with_slot(
        &self,
        expected_length: Option<usize>,
        reserved: Option<UploadSlot>,
    ) -> GatewayResult<BodyAdmissionLease> {
        let started = Instant::now();
        let mut slot = match (reserved, self.wait_queue()) {
            (None, Some(queue)) => Some(queue.try_slot(expected_length).ok_or_else(|| {
                gateway_telemetry::runtime::record_body_admission_decision(started.elapsed(), true);
                queue_rejection("concurrency")
            })?),
            (slot, _) => slot,
        };
        increment_bounded(&self.state.active_requests, 1, self.state.max_requests).map_err(
            |_| {
                gateway_telemetry::runtime::record_body_admission_decision(started.elapsed(), true);
                gateway_telemetry::record_body_admission_rejection("concurrency");
                GatewayError::GatewayOverloaded
            },
        )?;
        gateway_telemetry::runtime::record_body_admission_decision(started.elapsed(), false);
        gateway_telemetry::buffered_request_started();
        if let Some(slot) = &mut slot {
            slot.header_reservation.take();
        }
        let mut lease = BodyAdmissionLease {
            state: Arc::clone(&self.state),
            slot,
            reserved_bytes: 0,
            admitted_at: Instant::now(),
            expected_length,
            allocation_capacity: 0,
            rewrite_capacity: 0,
            working_bytes: 0,
            extent: 0,
            processing: self
                .state
                .size_policy
                .is_none_or(|policy| policy.elastic_max_requests.is_none()),
            compact_workspace: false,
            large: false,
        };
        lease.prepare_buffer(0, 0)?;
        Ok(lease)
    }

    pub fn size_policy(&self) -> Option<SizeAwareBodyPolicy> {
        self.state.size_policy
    }

    pub fn active_requests(&self) -> usize {
        self.state.active_requests.load(Ordering::Relaxed)
    }

    pub fn active_bytes(&self) -> usize {
        self.state.active_bytes.load(Ordering::Relaxed)
    }

    pub fn max_requests(&self) -> usize {
        self.state.max_requests
    }

    pub(crate) fn configured_max_requests(&self) -> usize {
        self.state.configured_max_requests
    }

    pub fn max_bytes(&self) -> usize {
        self.state.max_bytes
    }

    /// Instantaneous counters are sampled independently; admission still uses
    /// the same bounds. Waiting connections have no retained body.
    pub fn capacity_snapshot(&self) -> gateway_telemetry::runtime::BodyCapacitySnapshot {
        gateway_telemetry::runtime::BodyCapacitySnapshot::new(
            self.max_requests(),
            self.wait_queue().map_or_else(
                || self.active_requests(),
                |q| self.max_requests() - q.slots.available_permits(),
            ),
            self.max_bytes(),
            self.active_bytes(),
        )
    }
}

impl Default for BodyAdmissionController {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_BUFFERED_REQUESTS,
            DEFAULT_MAX_INFLIGHT_BUFFER_BYTES,
        )
        .expect("default body admission limits are valid")
    }
}

impl BodyAdmissionLease {
    /// Reserve forecast payload, allocation capacity, and analysis workspace
    /// before growing the buffer. Unknown lengths use shared (large) capacity.
    /// Growth is fail-fast; partial bodies never wait while holding memory.
    pub fn prepare_buffer(&mut self, length: usize, capacity: usize) -> GatewayResult<()> {
        let capacity = self.allocation_capacity.max(capacity);
        let extent = length.max(self.expected_length.unwrap_or(0));
        let large = self.expected_length.is_none() || extent > SMALL_BODY_MAX_BYTES;
        self.update_working(extent, capacity, large, self.processing)?;
        self.allocation_capacity = capacity;
        self.extent = extent;
        Ok(())
    }

    /// Upgrade before analysis, transformations or JSON materialization. Never
    /// wait holding a partial/full body when workspace is unavailable.
    pub fn begin_processing(&mut self) -> GatewayResult<()> {
        self.update_working(self.extent, self.allocation_capacity, self.large, true)?;
        self.processing = true;
        Ok(())
    }

    /// Output coexists with the input allocation and parsed structures. Keep
    /// their extent/capacity intact while independently reserving output growth.
    pub(crate) fn prepare_rewrite(&mut self, capacity: usize) -> GatewayResult<()> {
        let previous = self.rewrite_capacity;
        self.rewrite_capacity = previous.max(capacity);
        if let Err(error) =
            self.update_working(self.extent, self.allocation_capacity, self.large, true)
        {
            self.rewrite_capacity = previous;
            return Err(error);
        }
        self.processing = true;
        Ok(())
    }

    /// Retain a small metadata allowance after synchronous analysis, rather
    /// than holding body-sized scratch space across policy/database waits.
    /// Callers must keep full workspace while larger parsed structures live.
    pub(crate) fn retain_compact_workspace(&mut self) -> GatewayResult<()> {
        if self
            .state
            .size_policy
            .is_none_or(|policy| policy.elastic_max_requests.is_none())
        {
            return Ok(());
        }
        let previous = self.compact_workspace;
        self.compact_workspace = true;
        if let Err(error) =
            self.update_working(self.extent, self.allocation_capacity, self.large, false)
        {
            self.compact_workspace = previous;
            return Err(error);
        }
        self.processing = false;
        Ok(())
    }

    pub fn upload_remaining(&self) -> Option<std::time::Duration> {
        self.state.size_policy.map(|policy| {
            policy
                .upload_timeout
                .saturating_sub(self.admitted_at.elapsed())
        })
    }

    fn update_working(
        &mut self,
        length: usize,
        capacity: usize,
        large: bool,
        processing: bool,
    ) -> GatewayResult<()> {
        let Some(policy) = self.state.size_policy else {
            return Ok(());
        };
        let large = large || self.rewrite_capacity > SMALL_BODY_MAX_BYTES;
        let workspace = if processing || self.compact_workspace {
            let bytes = length
                .max(self.rewrite_capacity)
                .checked_mul(policy.workspace_multiplier)
                .ok_or(GatewayError::GatewayOverloaded)?;
            if processing {
                bytes
            } else {
                bytes.min(64 * 1024)
            }
        } else {
            0
        };
        let charge = workspace
            .checked_add(capacity.max(length))
            .and_then(|bytes| bytes.checked_add(self.rewrite_capacity))
            .and_then(|bytes| bytes.checked_add(REQUEST_WORKING_OVERHEAD))
            .ok_or(GatewayError::GatewayOverloaded)?;
        // Declared-length reservations usually cover many upload chunks. The
        // live lease already owns this charge; no shared update is needed.
        if charge == self.working_bytes && large == self.large {
            return Ok(());
        }
        let mut state = self
            .state
            .working
            .lock()
            .map_err(|_| GatewayError::GatewayOverloaded)?;
        let next_bytes = state
            .bytes
            .checked_sub(self.working_bytes)
            .and_then(|v| v.checked_add(charge));
        let next_large_bytes = state
            .large_bytes
            .checked_sub(if self.large { self.working_bytes } else { 0 })
            .and_then(|v| v.checked_add(if large { charge } else { 0 }));
        let next_large_requests =
            state.large_requests - usize::from(self.large) + usize::from(large);
        if next_bytes.is_none_or(|bytes| bytes > policy.max_working_bytes)
            || next_large_bytes.is_none_or(|bytes| {
                bytes > policy.max_working_bytes - policy.small_reserve().expect("validated policy")
            })
            || next_large_requests > self.state.max_requests - policy.reserved_small_requests
        {
            gateway_telemetry::record_body_admission_rejection(if processing && !self.processing {
                "processing_workspace"
            } else {
                "working_set"
            });
            return Err(GatewayError::GatewayOverloaded);
        }
        // A rewrite or an underestimated length can change the size class.
        // Promote without waiting while retaining a body, and release a large
        // permit when a completed small output no longer needs one.
        if let (Some(slot), Some(slots)) = (
            &mut self.slot,
            self.state
                .wait_queue
                .as_ref()
                .and_then(|q| q.large_slots.as_ref()),
        ) {
            if large && slot.large.is_none() {
                slot.large = Some(
                    slots
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| queue_rejection("working_set"))?,
                );
            } else if !large {
                slot.large.take();
            }
        }
        state.bytes = next_bytes.expect("checked above");
        state.large_bytes = next_large_bytes.expect("checked above");
        state.large_requests = next_large_requests;
        gateway_telemetry::body_working_bytes_changed(self.working_bytes, charge);
        self.working_bytes = charge;
        self.large = large;
        Ok(())
    }

    pub fn try_reserve(&mut self, additional_bytes: usize) -> GatewayResult<()> {
        if additional_bytes == 0 {
            return Ok(());
        }
        let started = Instant::now();
        increment_bounded(
            &self.state.active_bytes,
            additional_bytes,
            self.state.max_bytes,
        )
        .map_err(|_| {
            gateway_telemetry::runtime::record_body_admission_decision(started.elapsed(), true);
            gateway_telemetry::record_body_admission_rejection("bytes");
            GatewayError::GatewayOverloaded
        })?;
        gateway_telemetry::runtime::record_body_admission_decision(started.elapsed(), false);
        self.reserved_bytes = self.reserved_bytes.saturating_add(additional_bytes);
        gateway_telemetry::buffered_bytes_added(additional_bytes);
        Ok(())
    }

    pub fn reserved_bytes(&self) -> usize {
        self.reserved_bytes
    }

    /// Keep admission with the forwarded allocation until its last reference
    /// is dropped, including slices or clones queued behind upstream writes.
    pub(crate) fn into_bytes(mut self, body: Bytes) -> GatewayResult<Bytes> {
        // Guardrails may expand a body after its original chunks were admitted.
        self.try_reserve(body.len().saturating_sub(self.reserved_bytes))?;
        self.compact_workspace = false;
        let retained_capacity = if self.rewrite_capacity > 0 {
            self.rewrite_capacity
        } else {
            self.allocation_capacity
        };
        self.rewrite_capacity = 0;
        // Parsing/rewrite workspace is no longer needed, but the allocation
        // remains charged through all upstream clones/slices of the buffer.
        self.update_working(
            body.len(),
            retained_capacity.max(body.len()),
            body.len() > SMALL_BODY_MAX_BYTES,
            false,
        )?;
        Ok(Bytes::from_owner(AdmittedBody { body, _lease: self }))
    }
}

impl Drop for BodyAdmissionLease {
    fn drop(&mut self) {
        if self.working_bytes > 0 {
            let mut working = self
                .state
                .working
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            working.bytes -= self.working_bytes;
            if self.large {
                working.large_bytes -= self.working_bytes;
                working.large_requests -= 1;
            }
            gateway_telemetry::body_working_bytes_changed(self.working_bytes, 0);
        }
        if self.reserved_bytes > 0 {
            self.state
                .active_bytes
                .fetch_sub(self.reserved_bytes, Ordering::Relaxed);
            gateway_telemetry::buffered_bytes_removed(self.reserved_bytes);
        }
        self.state.active_requests.fetch_sub(1, Ordering::Relaxed);
        gateway_telemetry::buffered_request_finished();
        gateway_telemetry::runtime::record_body_retention(self.admitted_at.elapsed());
    }
}

fn increment_bounded(counter: &AtomicUsize, amount: usize, maximum: usize) -> Result<(), ()> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(amount).ok_or(())?;
        if next > maximum {
            return Err(());
        }
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(requests: usize, working_mib: usize, small: usize) -> BodyAdmissionController {
        BodyAdmissionController::with_size_policy(
            requests,
            512 * 1024 * 1024,
            Some(SizeAwareBodyPolicy {
                max_working_bytes: working_mib * 1024 * 1024,
                elastic_max_requests: None,
                reserved_small_requests: small,
                workspace_multiplier: 2,
                upload_timeout: std::time::Duration::from_secs(30),
            }),
        )
        .unwrap()
    }

    fn elastic(working_mib: usize, ceiling: usize) -> BodyAdmissionController {
        let mut policy = sized(2, working_mib, 0).size_policy().unwrap();
        policy.elastic_max_requests = Some(ceiling);
        BodyAdmissionController::with_size_policy(2, 512 * 1024 * 1024, Some(policy)).unwrap()
    }

    fn bounded_wait(max_waiters: usize, timeout: Duration) -> BodyAdmissionController {
        BodyAdmissionController::with_policies(
            1,
            1024,
            None,
            BodyWaitPolicy {
                max_waiters,
                timeout,
            },
        )
        .unwrap()
    }

    // Poll once on the current task to establish queue order without scheduling or sleeps.
    async fn assert_pending<F: std::future::Future>(future: std::pin::Pin<&mut F>) {
        tokio::select! {
            biased;
            _ = future => panic!("expected to wait for a slot"),
            _ = std::future::ready(()) => {}
        }
    }

    #[tokio::test]
    async fn queued_uploads_resume_in_order_after_the_final_forwarded_reference() {
        let controller = bounded_wait(2, Duration::from_secs(5));
        let queue = controller.wait_queue().unwrap();
        let held = controller
            .try_acquire()
            .unwrap()
            .into_bytes(Bytes::from_static(b"held"))
            .unwrap();
        let last = held.slice(1..);
        drop(held);
        let mut first = Box::pin(queue.wait_for_slot(None));
        let mut second = Box::pin(queue.wait_for_slot(None));
        assert_pending(first.as_mut()).await;
        assert_pending(second.as_mut()).await;
        assert!(matches!(
            queue.wait_for_slot(None).await,
            Err(GatewayError::GatewayOverloaded)
        ));
        assert_eq!(controller.active_requests(), 1);
        assert_eq!(controller.active_bytes(), 4);
        drop(last);
        assert!(
            controller.try_acquire().is_err(),
            "a post-call buffer must not jump the queue"
        );
        assert_pending(second.as_mut()).await;
        let slot = first.await.unwrap();
        let lease = controller.acquire_with_slot(None, Some(slot)).unwrap();
        assert_pending(second.as_mut()).await;
        drop(lease);
        drop(second.await.unwrap());
        assert_eq!(queue.waiters.available_permits(), 2);
        assert!(controller.try_acquire().is_ok());
    }

    #[tokio::test]
    async fn cancelled_or_timed_out_waits_return_their_places_without_body_charges() {
        let controller = bounded_wait(1, Duration::from_millis(20));
        let queue = controller.wait_queue().unwrap();
        let slot = queue.try_slot(None).unwrap();
        assert_eq!(controller.capacity_snapshot().free_requests, 0);
        let mut cancelled = Box::pin(queue.wait_for_slot(None));
        assert_pending(cancelled.as_mut()).await;
        drop(cancelled);
        assert_eq!(queue.waiters.available_permits(), 1);
        assert!(matches!(
            queue.wait_for_slot(None).await,
            Err(GatewayError::GatewayOverloaded)
        ));
        assert_eq!(queue.waiters.available_permits(), 1);
        assert_eq!(
            (controller.active_requests(), controller.active_bytes()),
            (0, 0)
        );
        drop(slot);
        assert!(queue.try_slot(None).is_some());
    }

    #[tokio::test]
    async fn waiting_large_uploads_leave_header_and_rewrite_capacity_for_small_bodies() {
        let controller = BodyAdmissionController::with_policies(
            2,
            32 << 20,
            sized(2, 32, 1).size_policy(),
            BodyWaitPolicy {
                max_waiters: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let queue = controller.wait_queue().unwrap();
        let large = queue.try_slot(None).unwrap();
        let mut waiting = Box::pin(queue.wait_for_slot(Some(SMALL_BODY_MAX_BYTES + 1)));
        assert_pending(waiting.as_mut()).await;
        let mut small = controller.try_acquire_sized(Some(1)).unwrap();
        assert!(small.prepare_rewrite(SMALL_BODY_MAX_BYTES + 1).is_err());
        assert_eq!(small.reserved_bytes(), 0);
        drop(large);
        drop(waiting.await.unwrap());
        small.prepare_rewrite(SMALL_BODY_MAX_BYTES + 1).unwrap();
        assert!(queue.try_slot(None).is_none());
        drop(small);
        assert!(queue.try_slot(None).is_some());
    }

    #[test]
    fn elastic_concurrency_follows_body_size_with_a_count_backstop() {
        let controller = elastic(8, 128);
        let mut small = Vec::new();
        while let Ok(lease) = controller.try_acquire_sized(Some(8192)) {
            small.push(lease);
        }
        assert!(small.len() > 100);
        assert!(
            small.len() < controller.max_requests(),
            "memory, not count, bound this mix"
        );
        drop(small);
        let mut large = Vec::new();
        while let Ok(lease) = controller.try_acquire_sized(Some(1024 * 1024)) {
            large.push(lease);
        }
        assert_eq!(large.len(), 7);
        drop(large);
        let controller = elastic(16, 128);
        let tiny: Vec<_> = (0..128)
            .map(|_| controller.try_acquire_sized(Some(1)).unwrap())
            .collect();
        assert!(controller.try_acquire_sized(Some(1)).is_err());
        drop(tiny);
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn elastic_processing_upgrade_is_atomic_and_forwarding_releases_workspace() {
        let controller = elastic(4, 32);
        let mut first = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        let second = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        let before = controller.state.working.lock().unwrap().bytes;
        assert!(first.begin_processing().is_err());
        assert!(!first.processing);
        assert_eq!(controller.state.working.lock().unwrap().bytes, before);
        drop(second);
        first.begin_processing().unwrap();
        let processing = first.working_bytes;
        first.begin_processing().unwrap();
        assert_eq!(
            first.working_bytes, processing,
            "upgrade must be idempotent"
        );
        let body = first.into_bytes(Bytes::from(vec![0; 1024 * 1024])).unwrap();
        let last = body.clone();
        drop(body);
        assert_eq!(
            controller.state.working.lock().unwrap().bytes,
            REQUEST_WORKING_OVERHEAD + 1024 * 1024
        );
        drop(last);
        assert_eq!(controller.active_bytes(), 0);
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn compact_metadata_releases_scratch_and_requires_reacquisition_before_processing() {
        let controller = elastic(4, 32);
        let mut lease = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        lease.begin_processing().unwrap();
        assert!(controller.try_acquire_sized(Some(1024 * 1024)).is_err());
        lease.retain_compact_workspace().unwrap();
        assert_eq!(lease.working_bytes, 1024 * 1024 + 2 * 64 * 1024);
        let other = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        assert!(lease.begin_processing().is_err());
        assert!(!lease.processing);
        drop(other);
        lease.begin_processing().unwrap();
        let bytes = lease.into_bytes(Bytes::from(vec![0; 1024 * 1024])).unwrap();
        assert_eq!(
            controller.state.working.lock().unwrap().bytes,
            1024 * 1024 + 64 * 1024
        );
        drop(bytes);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn elastic_growth_cannot_overdraw_memory_or_lose_existing_charge() {
        let controller = elastic(4, 32);
        let mut unknown = controller.try_acquire_sized(None).unwrap();
        unknown.prepare_buffer(1024 * 1024, 1024 * 1024).unwrap();
        let before = unknown.working_bytes;
        assert!(unknown
            .prepare_buffer(4 * 1024 * 1024, 4 * 1024 * 1024)
            .is_err());
        assert_eq!(unknown.working_bytes, before);
        assert_eq!(unknown.allocation_capacity, 1024 * 1024);
        drop(unknown);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn rewrite_growth_is_atomic_and_only_output_capacity_survives_forwarding() {
        let controller = elastic(4, 32);
        let mut lease = controller.try_acquire_sized(None).unwrap();
        lease.prepare_buffer(1024 * 1024, 1024 * 1024).unwrap();
        lease.begin_processing().unwrap();
        let input_charge = lease.working_bytes;
        lease.prepare_rewrite(128).unwrap();
        assert_eq!(lease.working_bytes, input_charge + 128);
        assert_eq!(lease.extent, 1024 * 1024);
        assert_eq!(lease.allocation_capacity, 1024 * 1024);
        assert!(lease.prepare_rewrite(2 * 1024 * 1024).is_err());
        assert!(lease.prepare_rewrite(usize::MAX).is_err());
        assert_eq!(lease.rewrite_capacity, 128);
        assert_eq!(lease.working_bytes, input_charge + 128);
        assert_eq!(
            controller.state.working.lock().unwrap().bytes,
            input_charge + 128
        );

        let body = lease.into_bytes(Bytes::from(vec![0; 128])).unwrap();
        let last = body.clone();
        drop(body);
        assert_eq!(
            controller.state.working.lock().unwrap().bytes,
            REQUEST_WORKING_OVERHEAD + 128
        );
        drop(last);
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(controller.active_bytes(), 0);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn expanded_rewrite_must_acquire_shared_capacity_before_leaving_small_reserve() {
        let controller = sized(2, 8, 1);
        let large = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        let mut small = controller.try_acquire_sized(Some(64)).unwrap();
        let before = small.working_bytes;
        assert!(small.prepare_rewrite(SMALL_BODY_MAX_BYTES + 1).is_err());
        assert_eq!(small.working_bytes, before);
        assert_eq!(small.rewrite_capacity, 0);
        assert!(!small.large);
        drop(large);
        small.prepare_rewrite(SMALL_BODY_MAX_BYTES + 1).unwrap();
        assert!(small.large);
        drop(small);
        let state = controller.state.working.lock().unwrap();
        assert_eq!(
            (state.bytes, state.large_bytes, state.large_requests),
            (0, 0, 0)
        );
    }

    #[test]
    fn elastic_memory_reserve_leaves_room_for_small_body_processing() {
        let mut policy = elastic(4, 32).size_policy().unwrap();
        policy.reserved_small_requests = 1;
        let controller =
            BodyAdmissionController::with_size_policy(2, 8 * 1024 * 1024, Some(policy)).unwrap();
        let large: Vec<_> = (0..3)
            .map(|_| controller.try_acquire_sized(Some(1024 * 1024)).unwrap())
            .collect();
        assert!(controller.try_acquire_sized(Some(1024 * 1024)).is_err());
        let mut small = controller
            .try_acquire_sized(Some(SMALL_BODY_MAX_BYTES))
            .unwrap();
        small.begin_processing().unwrap();
        drop((large, small));
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn concurrent_elastic_uploads_and_processing_keep_bounds_and_release_charges() {
        let controller = elastic(4, 32);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let controller = &controller;
                scope.spawn(move || {
                    for size in [8192, 64 * 1024, 1024 * 1024].into_iter().cycle().take(100) {
                        if let Ok(mut lease) = controller.try_acquire_sized(Some(size)) {
                            let _ = lease.begin_processing();
                            std::thread::yield_now();
                            assert!(controller.active_requests() <= controller.max_requests());
                            assert!(
                                controller.state.working.lock().unwrap().bytes <= 4 * 1024 * 1024
                            );
                        }
                    }
                });
            }
        });
        assert_eq!(controller.active_requests(), 0);
        let state = controller.state.working.lock().unwrap();
        assert_eq!(
            (state.bytes, state.large_bytes, state.large_requests),
            (0, 0, 0)
        );
    }

    #[test]
    fn large_and_unknown_uploads_cannot_consume_small_request_reserve() {
        let controller = sized(3, 32, 1);
        let large = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        let unknown = controller.try_acquire_sized(None).unwrap();
        assert!(controller.try_acquire_sized(Some(1024 * 1024)).is_err());
        let small = controller.try_acquire_sized(Some(8192)).unwrap();
        assert_eq!(controller.active_requests(), 3);
        drop((large, unknown, small));
        assert_eq!(controller.active_requests(), 0);
        let state = controller.state.working.lock().unwrap();
        assert_eq!(
            (state.bytes, state.large_bytes, state.large_requests),
            (0, 0, 0)
        );
    }

    #[test]
    fn declared_lengths_reserve_working_memory_without_claiming_actual_bytes() {
        let controller = sized(32, 8, 2);
        let first = controller.try_acquire_sized(Some(2 * 1024 * 1024)).unwrap();
        assert!(controller.try_acquire_sized(Some(2 * 1024 * 1024)).is_err());
        assert_eq!(controller.active_bytes(), 0);
        assert_eq!(controller.active_requests(), 1);
        let small = controller
            .try_acquire_sized(Some(SMALL_BODY_MAX_BYTES))
            .unwrap();
        drop((first, small));
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn an_underestimated_small_body_must_promote_before_growing() {
        let controller = sized(2, 32, 1);
        let large = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        let mut small = controller.try_acquire_sized(Some(1024)).unwrap();
        let before = small.working_bytes;
        assert!(small
            .prepare_buffer(SMALL_BODY_MAX_BYTES + 1, SMALL_BODY_MAX_BYTES * 2)
            .is_err());
        assert_eq!(small.working_bytes, before);
        drop(large);
        small
            .prepare_buffer(SMALL_BODY_MAX_BYTES + 1, SMALL_BODY_MAX_BYTES * 2)
            .unwrap();
        assert!(small.large);
        drop(small);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn upload_workspace_is_released_but_forwarded_allocation_stays_charged() {
        let controller = sized(4, 32, 0);
        let mut lease = controller.try_acquire_sized(Some(1024 * 1024)).unwrap();
        lease.prepare_buffer(1024 * 1024, 2 * 1024 * 1024).unwrap();
        let during = lease.working_bytes;
        let body = lease.into_bytes(Bytes::from(vec![0; 1024 * 1024])).unwrap();
        let queued = body.slice(0..1);
        drop(body);
        let retained = controller.state.working.lock().unwrap().bytes;
        assert_eq!(during - retained, 2 * 1024 * 1024);
        assert_eq!(retained, REQUEST_WORKING_OVERHEAD + 2 * 1024 * 1024);
        drop(queued);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
        assert_eq!(controller.active_bytes(), 0);
    }

    #[test]
    fn working_charge_overflow_fails_closed_without_leaking_a_slot() {
        let controller = sized(2, 8, 0);
        assert!(controller.try_acquire_sized(Some(usize::MAX)).is_err());
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(controller.state.working.lock().unwrap().bytes, 0);
    }

    #[test]
    fn defaults_match_the_documented_memory_budget() {
        let controller = BodyAdmissionController::default();

        assert_eq!(controller.max_requests(), 8);
        assert_eq!(controller.max_bytes(), 536_870_912);
    }

    #[test]
    fn capacity_reports_effective_limits_and_releases_failed_reservations() {
        let controller = BodyAdmissionController::new(2, 10).unwrap();
        let mut lease = controller.try_acquire().unwrap();
        lease.try_reserve(6).unwrap();
        let snapshot = controller.capacity_snapshot();
        assert_eq!(snapshot.max_requests, 2);
        assert_eq!(snapshot.used_requests, 1);
        assert_eq!(snapshot.free_requests, 1);
        assert_eq!(snapshot.max_bytes, 10);
        assert_eq!(snapshot.used_bytes, 6);
        assert_eq!(snapshot.free_bytes, 4);
        assert!(lease.try_reserve(5).is_err());
        assert_eq!(controller.capacity_snapshot(), snapshot);
        drop(lease);
        assert_eq!(controller.capacity_snapshot().free_requests, 2);
        assert_eq!(controller.capacity_snapshot().free_bytes, 10);
    }

    #[test]
    fn bounds_concurrent_buffered_requests_and_releases_on_drop() {
        let controller = BodyAdmissionController::new(1, 16).expect("controller");
        let lease = controller.try_acquire().expect("first lease");

        assert_eq!(controller.active_requests(), 1);
        assert_eq!(
            controller.try_acquire().unwrap_err(),
            GatewayError::GatewayOverloaded
        );

        drop(lease);
        assert_eq!(controller.active_requests(), 0);
        assert!(controller.try_acquire().is_ok());
    }

    #[test]
    fn bounds_aggregate_bytes_without_leaking_failed_reservations() {
        let controller = BodyAdmissionController::new(2, 10).expect("controller");
        let mut first = controller.try_acquire().expect("first lease");
        let mut second = controller.try_acquire().expect("second lease");

        first.try_reserve(6).expect("first reservation");
        assert_eq!(
            second.try_reserve(5).unwrap_err(),
            GatewayError::GatewayOverloaded
        );
        assert_eq!(controller.active_bytes(), 6);
        assert_eq!(second.reserved_bytes(), 0);

        second.try_reserve(4).expect("remaining capacity");
        assert_eq!(controller.active_bytes(), 10);
        drop(first);
        assert_eq!(controller.active_bytes(), 4);
        drop(second);
        assert_eq!(controller.active_bytes(), 0);
    }

    #[test]
    fn forwarded_clones_and_slices_keep_admission_until_the_last_reference() {
        let controller = BodyAdmissionController::new(1, 8).expect("controller");
        let body = Bytes::from(vec![1, 2, 3, 4]);
        let original_ptr = body.as_ptr();
        let forwarded = controller
            .try_acquire()
            .unwrap()
            .into_bytes(body)
            .expect("admitted body");
        assert_eq!(
            forwarded.as_ptr(),
            original_ptr,
            "do not copy the allocation"
        );
        let queued = forwarded.clone();
        let slice = forwarded.slice(1..3);
        drop(forwarded);
        drop(queued);

        assert_eq!(slice.as_ref(), &[2, 3]);
        assert_eq!(controller.active_requests(), 1);
        assert_eq!(controller.active_bytes(), 4);
        assert_eq!(
            controller.try_acquire().unwrap_err(),
            GatewayError::GatewayOverloaded
        );

        drop(slice);
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(controller.active_bytes(), 0);
        assert!(controller.try_acquire().is_ok());
    }

    #[test]
    fn accounts_for_rewritten_body_growth_and_releases_failed_admission() {
        let controller = BodyAdmissionController::new(1, 8).expect("controller");
        let mut lease = controller.try_acquire().unwrap();
        lease.try_reserve(4).unwrap();
        let forwarded = lease.into_bytes(Bytes::from(vec![0; 8])).unwrap();
        assert_eq!(controller.active_bytes(), 8);
        drop(forwarded);
        assert_eq!(controller.active_bytes(), 0);

        let mut lease = controller.try_acquire().unwrap();
        lease.try_reserve(4).unwrap();
        assert_eq!(
            lease.into_bytes(Bytes::from(vec![0; 9])).unwrap_err(),
            GatewayError::GatewayOverloaded
        );
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(controller.active_bytes(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_queued_write_releases_its_body_admission() {
        let controller = BodyAdmissionController::new(1, 8).expect("controller");
        let body = controller
            .try_acquire()
            .unwrap()
            .into_bytes(Bytes::from(vec![0; 8]))
            .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _queued_body = body;
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        assert_eq!(controller.active_requests(), 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(controller.active_bytes(), 0);
    }
}
