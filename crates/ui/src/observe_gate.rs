//! Last-value gate for `AppState` observers: a view derives the (small) slice
//! of state its render reads, and repaints only when that slice differs from
//! the one it last saw. Use it for slices that are cheap to build and compare;
//! large ones want a hash like `AppState::overview_fingerprint`.

#[derive(Debug)]
pub(crate) struct ObserveGate<T>(Option<T>);

impl<T> Default for ObserveGate<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T: PartialEq> ObserveGate<T> {
    /// Record `slice`; true when it differs from the previous one (the first
    /// call always reports a change).
    pub(crate) fn changed(&mut self, slice: T) -> bool {
        if self.0.as_ref() == Some(&slice) {
            return false;
        }
        self.0 = Some(slice);
        true
    }

    /// Seed with the slice the view's first render will read.
    pub(crate) fn seeded(slice: T) -> Self {
        Self(Some(slice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_real_changes() {
        let mut gate = ObserveGate::default();
        assert!(gate.changed(1));
        assert!(!gate.changed(1));
        assert!(gate.changed(2));
        assert!(!ObserveGate::seeded(3).changed(3));
    }
}
