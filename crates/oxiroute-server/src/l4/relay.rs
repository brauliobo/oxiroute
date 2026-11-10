//! TCP relay ends and the kernel-assisted bulk path.

pub(crate) mod end;
pub(in crate::l4) mod splice;

#[cfg(test)]
mod tests;
