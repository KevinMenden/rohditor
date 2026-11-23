//! Captures device errors around a block of GPU work so callers can report them as ordinary
//! failures instead of hitting the device's uncaptured-error handler.

/// Nested validation, out-of-memory and internal error scopes on one device.
///
/// wgpu requires scopes to end innermost first. Fields drop in declaration order, so an early
/// return that skips [`ErrorScopes::finish`] unwinds them correctly too.
pub(crate) struct ErrorScopes {
    validation: wgpu::ErrorScopeGuard,
    allocation: wgpu::ErrorScopeGuard,
    internal: wgpu::ErrorScopeGuard,
}

impl ErrorScopes {
    pub fn push(device: &wgpu::Device) -> Self {
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let allocation = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        Self {
            validation,
            allocation,
            internal,
        }
    }

    /// Close all scopes and return the most specific error.
    pub fn finish(self) -> Option<wgpu::Error> {
        let validation = pollster::block_on(self.validation.pop());
        let allocation = pollster::block_on(self.allocation.pop());
        let internal = pollster::block_on(self.internal.pop());
        validation.or(allocation).or(internal)
    }
}
