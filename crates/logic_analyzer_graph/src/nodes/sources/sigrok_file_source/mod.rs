#[cfg_attr(target_arch = "wasm32", path = "builder_wasm.rs")]
mod builder;
mod definition;
mod registration;

#[cfg(test)]
pub(crate) use definition::SigrokFileSource;
