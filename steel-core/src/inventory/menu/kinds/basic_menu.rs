use crate::inventory::menu::MenuKind;

/// A menu kind with all-default handling and no special behavior.
#[derive(Debug)]
pub struct BasicKind {}
impl MenuKind for BasicKind {}
