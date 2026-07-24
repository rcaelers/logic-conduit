//! Native host infrastructure for Sigrok Python decoders without libsigrokdecode.

#[allow(dead_code)]
mod bridge;
#[allow(dead_code)]
mod conditions;
#[cfg(test)]
mod feasibility;
#[allow(dead_code)]
mod python_host;
#[allow(dead_code)]
mod scheduler;
#[allow(dead_code)]
mod worker;

#[cfg(test)]
mod worker_tests;
