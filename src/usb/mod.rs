pub mod constants;
pub mod descriptors;
pub mod pma;
pub mod hid;
pub mod usb_ext;

use core::cmp::min;
use rtfm::Threshold;

use stm32l151;

use self::constants::{UsbDescriptorType, UsbRequest};
use self::pma::PMA;
use self::usb_ext::UsbEpExt;
use hidreport::HidReport;
use usb::hid::UsbHid;

const MAX_PACKET_SIZE: u32 = 64;

pub struct Usb {
    usb: stm32l151::USB,
    pending_daddr: u8,
    pma: &'static mut PMA,
    hid: UsbHid,
}

impl Usb {
    pub fn new(
        usb: stm32l151::USB,
        rcc: &mut stm32l151::RCC,
        syscfg: &mut stm32l151::SYSCFG,
    ) -> Usb {
        let pma = unsafe { &mut *PMA.get() };
        pma.zero();

        rcc.apb1enr.modify(|_, w| w.usben().set_bit());
        rcc.apb1rstr.modify(|_, w| w.usbrst().set_bit());
        rcc.apb1rstr.modify(|_, w| w.usbrst().clear_bit());

        usb.usb_cntr.modify(|_, w| w.pdwn().clear_bit());
        usb.usb_cntr.modify(|_, w| {
            w.ctrm().set_bit()
             .errm().set_bit()
             .pmaovrm().set_bit()
             //.wkupm().set_bit()
             //.suspm().set_bit()
             //.esofm().set_bit()
             //.sofm().set_bit()
             .resetm().set_bit()
        });
        usb.btable.reset();
        usb.usb_cntr.modify(|_, w| w.fres().clear_bit());
        usb.istr.reset();
        usb.daddr.modify(|_, w| w.ef().set_bit());

        syscfg.pmc.modify(|_, w| w.usb_pu().set_bit());

        let hid = hid::UsbHid::new();

        Usb {
            usb,
            pending_daddr: 0,
            pma,
            hid,
        }
    }

    pub fn update_report(&mut self, report: &HidReport) {
        self.hid.report[..].clone_from_slice(report.as_bytes());
    }

    pub fn interrupt(&mut self) {
        let istr = self.usb.istr.read();
        if istr.reset().bit_is_set() {
            self.usb.istr.modify(|_, w| w.reset().clear_bit());
            self.reset();
        }

        self.usb
            .istr
            .modify(|_, w| w.susp().clear_bit().sof().clear_bit().esof().clear_bit());
        let istr = self.usb.istr.read();
        if istr.ctr().bit_is_set() {
            self.usb.istr.modify(|_, w| w.ctr().clear_bit());

            let endpoint = istr.ep_id().bits();
            match endpoint {
                0 => {
                    self.ctr();
                }
                1 => {
                    self.hid.ctr(&mut self.usb, &mut self.pma);
                }
                _ => panic!(),
            }
        }

        let istr = self.usb.istr.read();
        if istr.bits() != 0 {
            //debug!("nz {:x}", istr.bits());
        }
    }

    fn reset(&mut self) {
        self.pma.pma_area.set_u16(0, 0x40);
        self.pma.pma_area.set_u16(2, 0x0);
        self.pma.pma_area.set_u16(4, 0x20);
        self.pma
            .pma_area
            .set_u16(6, (0x8000 | ((MAX_PACKET_SIZE / 32) - 1) << 10) as u16);
        self.pma.pma_area.set_u16(8, 0x100);
        self.pma.pma_area.set_u16(10, 0x0);

        self.pma.write_buffer_u8(0x100, &self.hid.report);
        self.pma.pma_area.set_u16(10, 5);

        self.usb.usb_ep0r.modify(|_, w| unsafe {
            w.ep_type()
                .bits(0b01)
                .stat_tx()
                .bits(0b10)
                .stat_rx()
                .bits(0b11)
        });

        self.usb.usb_ep1r.modify(|_, w| unsafe {
            w.ep_type()
                .bits(0b11)
                .stat_tx()
                .bits(0b11)
                .stat_rx()
                .bits(0b10)
                .ea()
                .bits(0b1)
        });

        self.usb.daddr.modify(|_, w| w.ef().set_bit());
    }

    fn ctr(&mut self) {
        if self.usb.istr.read().dir().bit_is_set() {
            self.rx()
        } else {
            self.tx()
        }
    }

    fn tx(&mut self) {
        if self.pending_daddr != 0 {
            self.usb
                .daddr
                .modify(|_, w| unsafe { w.add().bits(self.pending_daddr) });
        } else {
            self.pma.pma_area.set_u16(6, 0);
        }

        self.usb.usb_ep0r.toggle_tx_out();
    }

    fn get_device_descriptor(&mut self, value: u16, length: u16) {
        let descriptor_type = UsbDescriptorType::from((value >> 8) as u8);
        let index = (value & 0xff) as u8;
        let descriptor: Option<&[u8]> = match descriptor_type {
            UsbDescriptorType::Configuration => Some(&descriptors::CONF_DESC),
            UsbDescriptorType::Device => Some(&descriptors::DEV_DESC),
            UsbDescriptorType::DeviceQualifier => Some(&descriptors::DEVICE_QUALIFIER),
            UsbDescriptorType::StringDesc => match index {
                0 => Some(&descriptors::LANG_STR),
                1 => Some(&descriptors::MANUFACTURER_STR),
                2 => Some(&descriptors::PRODUCT_STR),
                3 => Some(&descriptors::SERIAL_NUMBER_STR),
                4 => Some(&descriptors::CONF_STR),
                _ => None,
            },
            UsbDescriptorType::Debug => None,
            _ => {
                debug!("get descriptor {:x}", value).ok();
                None
            }
        };
        match descriptor {
            Some(bytes) => {
                self.pma.write_buffer_u8(0x40, bytes);
                self.pma
                    .pma_area
                    .set_u16(2, min(length, bytes.len() as u16));
                self.usb.usb_ep0r.toggle_out();
            }
            None => self.usb.usb_ep0r.toggle_tx_stall(),
        }
    }

    fn rx(&mut self) {
        let request16 = self.pma.pma_area.get_u16(0x20);
        let value = self.pma.pma_area.get_u16(0x22);
        //let index = self.pma.pma_area.get_u16(0x24);
        let length = self.pma.pma_area.get_u16(0x26);

        self.pma
            .pma_area
            .set_u16(6, (0x8000 | ((MAX_PACKET_SIZE / 32) - 1) << 10) as u16);

        let request = UsbRequest::from(((request16 & 0xff00) >> 8) as u8);
        let request_type = (request16 & 0xff) as u8;
        match (request_type, request) {
            (0x00, UsbRequest::SetAddress) => {
                self.pending_daddr = value as u8;
                self.usb.usb_ep0r.toggle_0();
            }
            (0x00, UsbRequest::SetConfiguration) => {
                // TODO: check value?
                self.pma.pma_area.set_u16(2, 0);
                self.usb.usb_ep0r.toggle_0();
            }
            (0x00, UsbRequest::GetStatus) => {
                self.pma.pma_area.set_u16(0x40, 0);
                self.pma.pma_area.set_u16(2, 2);
                self.usb.usb_ep0r.toggle_out();
            }
            (0x80, UsbRequest::GetDescriptor) => {
                self.get_device_descriptor(value, length);
            }
            (0x81, UsbRequest::GetDescriptor) => {
                let descriptor_type = UsbDescriptorType::from((value >> 8) as u8);
                match descriptor_type {
                    UsbDescriptorType::HidReport => {
                        self.pma
                            .write_buffer_u8(0x40, &descriptors::HID_REPORT_DESC);
                        self.pma
                            .pma_area
                            .set_u16(2, min(length, descriptors::HID_REPORT_DESC.len() as u16));
                        self.usb.usb_ep0r.toggle_out();
                    }
                    _ => panic!(),
                }
            }
            (0x21, UsbRequest::GetInterface) => {
                self.pma.pma_area.set_u16(2, 0);
                self.usb.usb_ep0r.toggle_out();
            }
            (0x21, UsbRequest::SetInterface) => {
                self.pma.pma_area.set_u16(2, 0);
                self.usb.usb_ep0r.toggle_0();
            }
            (0x21, UsbRequest::SetConfiguration) => {
                // TODO: is this correct?
                self.pma.pma_area.set_u16(2, 0);
                self.usb.usb_ep0r.toggle_0();
            }
            _ => {
                debug!("rt {:x} {:?}", request_type, request).ok();
                panic!();
            }
        }
    }
}

pub fn usb_lp(_t: &mut Threshold, mut r: super::USB_LP::Resources) {
    r.USB.interrupt()
}
