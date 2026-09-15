//! Conservative processing-allocation reservations across preview and export
//! devices. Driver pools, shader code, and UI renderer allocations are excluded;
//! this must never be reported as measured physical GPU memory.
use std::sync::atomic::{AtomicU64, Ordering};

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
    pub fn new(bytes: u64) -> Self {
        let total = RESERVED.fetch_add(bytes, Ordering::AcqRel) + bytes;
        PEAK.fetch_max(total, Ordering::AcqRel);
        Self(bytes)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        RESERVED.fetch_sub(self.0, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simultaneous_workers_contribute_to_combined_peak() {
        let before = gpu_memory_reservations();
        let preview = Reservation::new(100);
        let export = Reservation::new(200);
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
}
