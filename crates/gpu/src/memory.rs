//! Conservative processing-allocation reservations across preview and export
//! devices. Driver pools, shader code, and UI renderer allocations are excluded;
//! this must never be reported as measured physical GPU memory.
use std::sync::atomic::{AtomicU64, Ordering};

use crate::GpuPreviewError;

static RESERVED: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default)]
pub struct GpuMemoryReservations {
    pub current_bytes: u64,
    /// Highest combined reservation observed since process startup.
    pub peak_bytes: u64,
}

pub fn gpu_memory_reservations() -> GpuMemoryReservations {
    GpuMemoryReservations {
        current_bytes: RESERVED.load(Ordering::Acquire),
        peak_bytes: PEAK.load(Ordering::Acquire),
    }
}

pub(crate) struct Reservation(u64);

impl Reservation {
    /// Reserve bytes before creating a GPU resource.
    ///
    /// Resource owners live on the preview UI thread, the background preview
    /// worker, and the export worker. A process-wide compare-and-swap is the
    /// one place that can see both the outgoing display frame and the worker's
    /// replacement allocation. This is deliberately a conservative logical
    /// budget, not a report of driver-allocated memory.
    pub fn try_new(bytes: u64, budget: u64) -> Result<Self, GpuPreviewError> {
        let mut current = RESERVED.load(Ordering::Acquire);
        loop {
            let requested =
                current
                    .checked_add(bytes)
                    .ok_or_else(|| GpuPreviewError::Unsupported {
                        reason: "GPU memory reservation overflowed".to_owned(),
                    })?;
            if requested > budget {
                return Err(GpuPreviewError::Unsupported {
                    reason: format!(
                        "GPU working-set budget exhausted: {current} resident bytes plus {bytes} requested bytes exceed the {budget}-byte limit"
                    ),
                });
            }
            match RESERVED.compare_exchange_weak(
                current,
                requested,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    PEAK.fetch_max(requested, Ordering::AcqRel);
                    return Ok(Self(bytes));
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// Release a completed operation's transient allocation while retaining
    /// ownership of its resident result.
    ///
    /// Callers must reserve their complete peak before creating resources, then
    /// reduce the reservation only after the transient resources have been
    /// dropped and no submitted work still references them.
    pub(crate) fn release_to(&mut self, resident_bytes: u64) {
        let released = self
            .0
            .checked_sub(resident_bytes)
            .expect("resident reservation cannot exceed its original peak");
        self.0 = resident_bytes;
        RESERVED.fetch_sub(released, Ordering::AcqRel);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        RESERVED.fetch_sub(self.0, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // The production counter is process-global so preview and export can share
    // one budget. These fixtures choose tiny budgets relative to its current
    // value, therefore they must not overlap each other under the test runner.
    static RESERVATION_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn simultaneous_workers_contribute_to_combined_peak() {
        let _guard = RESERVATION_TEST_LOCK
            .lock()
            .expect("test lock is available");
        let before = gpu_memory_reservations();
        let preview = Reservation::try_new(100, before.current_bytes + 300)
            .expect("preview reservation should fit");
        let export = Reservation::try_new(200, before.current_bytes + 300)
            .expect("export reservation should fit");
        assert_eq!(
            gpu_memory_reservations().current_bytes,
            before.current_bytes + 300
        );
        assert!(gpu_memory_reservations().peak_bytes >= before.current_bytes + 300);
        drop(export);
        drop(preview);
        assert_eq!(
            gpu_memory_reservations().current_bytes,
            before.current_bytes
        );
    }

    #[test]
    fn reservation_refuses_to_displace_an_outgoing_frame() {
        let _guard = RESERVATION_TEST_LOCK
            .lock()
            .expect("test lock is available");
        let before = gpu_memory_reservations();
        let current_frame = Reservation::try_new(100, before.current_bytes + 150)
            .expect("current frame should fit");
        assert!(Reservation::try_new(51, before.current_bytes + 150).is_err());
        drop(current_frame);
        assert_eq!(
            gpu_memory_reservations().current_bytes,
            before.current_bytes
        );
    }

    #[test]
    fn reservation_releases_transient_bytes_before_its_resident_result_drops() {
        let _guard = RESERVATION_TEST_LOCK
            .lock()
            .expect("test lock is available");
        let before = gpu_memory_reservations();
        let mut result = Reservation::try_new(300, before.current_bytes + 300)
            .expect("peak reservation should fit");
        result.release_to(100);
        assert_eq!(
            gpu_memory_reservations().current_bytes,
            before.current_bytes + 100
        );
        drop(result);
        assert_eq!(
            gpu_memory_reservations().current_bytes,
            before.current_bytes
        );
    }
}
