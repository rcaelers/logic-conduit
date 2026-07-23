static LINK_ANCHOR: u8 = 0;

/// Keeps this compile-time node bundle and its inventory submissions linked into a host.
#[inline(never)]
pub fn link() -> usize {
    std::ptr::addr_of!(LINK_ANCHOR) as usize
}
