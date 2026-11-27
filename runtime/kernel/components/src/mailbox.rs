// Licensed under the Apache-2.0 license

// Component for mailbox capsule.

use caliptra_api::SocManager;
use caliptra_mcu_capsules_runtime::dma::hil::Dma as DmaHal;
use caliptra_mcu_capsules_runtime::mailbox::Mailbox;
use capsules_core::virtualizers::virtual_alarm::{MuxAlarm, VirtualMuxAlarm};
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use kernel::capabilities;
use kernel::component::Component;
use kernel::create_capability;
use kernel::hil::time::Alarm;

#[macro_export]
macro_rules! mailbox_component_static {
    // `$S` is the platform's Caliptra SoC interface, `$staging_addr` the
    // optional AXI address of the staging SRAM.
    ($A:ty, $S:ty, $staging_addr:expr) => {{
        use capsules_core::virtualizers::virtual_alarm::VirtualMuxAlarm;
        let alarm = kernel::static_buf!(VirtualMuxAlarm<'static, $A>);
        let caliptra_soc = kernel::static_buf!($S);
        let mbox = kernel::static_buf!(
            caliptra_mcu_capsules_runtime::mailbox::Mailbox<
                'static,
                VirtualMuxAlarm<'static, $A>,
                $S,
            >
        );
        (alarm, mbox, caliptra_soc, $staging_addr)
    }};
}

pub struct MailboxComponent<A: Alarm<'static> + 'static, S: SocManager + Default + 'static> {
    board_kernel: &'static kernel::Kernel,
    driver_num: usize,
    mux_alarm: &'static MuxAlarm<'static, A>,
    dma: &'static dyn DmaHal,
    timeout_ticks: Option<u32>,
    _soc: PhantomData<S>,
}

impl<A: Alarm<'static>, S: SocManager + Default + 'static> MailboxComponent<A, S> {
    pub fn new(
        board_kernel: &'static kernel::Kernel,
        driver_num: usize,
        mux_alarm: &'static MuxAlarm<'static, A>,
        dma: &'static dyn DmaHal,
        timeout_ticks: Option<u32>,
    ) -> Self {
        Self {
            board_kernel,
            driver_num,
            mux_alarm,
            dma,
            timeout_ticks,
            _soc: PhantomData,
        }
    }
}

impl<A: Alarm<'static>, S: SocManager + Default + 'static> Component for MailboxComponent<A, S> {
    type StaticInput = (
        &'static mut MaybeUninit<VirtualMuxAlarm<'static, A>>,
        &'static mut MaybeUninit<Mailbox<'static, VirtualMuxAlarm<'static, A>, S>>,
        &'static mut MaybeUninit<S>,
        Option<u64>,
    );

    type Output = &'static Mailbox<'static, VirtualMuxAlarm<'static, A>, S>;

    fn finalize(self, static_buffer: Self::StaticInput) -> Self::Output {
        let mux_alarm = static_buffer.0.write(VirtualMuxAlarm::new(self.mux_alarm));
        mux_alarm.setup();

        let caliptra_soc = static_buffer.2.write(S::default());

        let grant_cap = create_capability!(capabilities::MemoryAllocationCapability);
        let mailbox: &Mailbox<'_, VirtualMuxAlarm<'_, _>, S> =
            static_buffer
                .1
                .write(caliptra_mcu_capsules_runtime::mailbox::Mailbox::new(
                    mux_alarm,
                    self.board_kernel.create_grant(self.driver_num, &grant_cap),
                    caliptra_soc,
                    static_buffer.3,
                    self.dma,
                    self.timeout_ticks,
                ));
        mailbox
    }
}
