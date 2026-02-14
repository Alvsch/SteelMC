#![expect(missing_docs)]

use stabby::dynptr;
use stabby::sync::Arc;

#[stabby::stabby(checked)]
pub trait Player {
    extern "C" fn greet(&self);
}

#[stabby::stabby]
pub struct Plugin {
    pub on_enable: extern "C" fn(dynptr!(Arc<dyn Player>)),
}
