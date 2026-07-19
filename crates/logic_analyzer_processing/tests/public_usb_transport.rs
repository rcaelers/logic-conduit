#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use logic_analyzer_processing::{DsLogicU3Pro16, LinkSpeed, UsbError, UsbTransport};

struct ExternalTransport;

impl UsbTransport for ExternalTransport {
    fn link_speed(&self) -> LinkSpeed {
        LinkSpeed::Super
    }

    fn control_write(
        &mut self,
        _request_type: u8,
        _request: u8,
        _value: u16,
        _index: u16,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, UsbError> {
        Ok(data.len())
    }

    fn control_read(
        &mut self,
        _request_type: u8,
        _request: u8,
        _value: u16,
        _index: u16,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, UsbError> {
        Ok(data.len())
    }

    fn bulk_write(
        &mut self,
        _endpoint: u8,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, UsbError> {
        Ok(data.len())
    }

    fn bulk_read(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, UsbError> {
        Ok(data.len())
    }
}

#[test]
fn external_crate_can_name_and_implement_the_transport_contract() {
    let device = DsLogicU3Pro16::new(ExternalTransport);
    assert!(device.is_ok());
}
