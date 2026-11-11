//! Reservations follow the actual IO/file owner, including after observer cancellation.
use std::sync::{Arc, Mutex, LazyLock};
pub(crate) const MAX_TEMPORARY_BYTES: u64 = 16 * 1024 * 1024 * 1024;
static TEMPORARY: LazyLock<Arc<ByteBudget>> = LazyLock::new(Arc::default);
pub(crate) fn temporary_budget() -> Arc<ByteBudget> {
    TEMPORARY.clone()
}

#[derive(Default)]
pub(crate) struct ByteBudget(Mutex<u64>);
impl ByteBudget {
    pub fn reserve(self: &Arc<Self>, bytes: u64, maximum: u64) -> Result<Reservation, String> {
        let mut used = self.0.lock().unwrap();
        if bytes > maximum.saturating_sub(*used) {
            return Err("Attachment storage or memory budget is full".into());
        }
        *used += bytes;
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
    pub fn used(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}
pub(crate) struct Reservation {
    budget: Arc<ByteBudget>,
    bytes: u64,
}
impl Reservation {
    pub fn grow(&mut self, bytes: u64, maximum: u64) -> Result<(), String> {
        let mut extra = self.budget.reserve(bytes, maximum)?;
        self.bytes += bytes;
        extra.bytes = 0;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        *self.budget.0.lock().unwrap() -= self.bytes;
    }
}
