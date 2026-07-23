use crate::inventory::menu::MenuKind;

/// A basic menu kind that does everything vanilla with no special behavior added
#[derive(Debug)]
pub struct BasicKind {}
impl MenuKind for BasicKind {}
