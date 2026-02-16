use crate::StableAbi;
use std::ops::{Deref, DerefMut};

#[repr(transparent)]
#[derive(StableAbi)]
pub struct Uuid(uuid::Bytes);

impl Uuid {
    pub fn new(uuid: uuid::Uuid) -> Self {
        Self(uuid.into_bytes())
    }
}

impl Deref for Uuid {
    type Target = uuid::Uuid;

    fn deref(&self) -> &Self::Target {
        // SAFETY: Uuid has the same memory layout as `uuid::Uuid`
        unsafe { &*(self.0.as_ptr() as *const uuid::Uuid) }
    }
}

impl DerefMut for Uuid {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: Uuid has the same memory layout as `uuid::Uuid`
        unsafe { &mut *(self.0.as_mut_ptr() as *mut uuid::Uuid) }
    }
}
