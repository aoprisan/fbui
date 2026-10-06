//! Time and sleep: the ARM generic timer for `now_ms`, and `wfi` woken by
//! either a UART receive interrupt or a timer compare — so an idle viewer
//! really idles (fbui's 0%-CPU rule), with interrupts masked throughout:
//! `wfi` wakes on a *pending* interrupt even when `DAIF.I` is set, and we
//! never take the exception.

use core::arch::asm;
use core::ptr::write_volatile;

/// BCM2835 interrupt controller: "Enable IRQs 2" (GPU IRQs 32–63).
const ARMCTRL_ENABLE_IRQS_2: usize = 0x3F00_B214;
/// IRQ 57 = PL011 UART.
const UART_IRQ_BIT: u32 = 1 << (57 - 32);
/// BCM2836 local interrupt controller: core 0 timer interrupt routing.
const CORE0_TIMER_IRQCNTL: usize = 0x4000_0040;

fn freq() -> u64 {
    let f: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f) };
    f.max(1)
}

fn ticks() -> u64 {
    let t: u64;
    unsafe { asm!("isb", "mrs {}, cntpct_el0", out(reg) t) };
    t
}

pub fn now_ms() -> u64 {
    ticks() * 1000 / freq()
}

/// Route the wake sources to core 0.
pub fn init_wake() {
    unsafe {
        write_volatile(ARMCTRL_ENABLE_IRQS_2 as *mut u32, UART_IRQ_BIT);
        // Secure and non-secure physical timer → core 0 IRQ.
        write_volatile(CORE0_TIMER_IRQCNTL as *mut u32, 0b11);
        asm!("msr daifset, #2"); // keep IRQs masked: wake only, no handler
    }
}

/// Sleep until UART input or `deadline_ms`, whichever comes first (no
/// deadline: until input). May return early; callers re-check.
pub fn sleep_until(deadline_ms: Option<u64>, input_ready: impl Fn() -> bool) {
    if let Some(d) = deadline_ms {
        if now_ms() >= d {
            return;
        }
        let cval = d * freq() / 1000;
        unsafe {
            asm!("msr cntp_cval_el0, {}", "msr cntp_ctl_el0, {}", "isb", in(reg) cval, in(reg) 1u64)
        };
    }
    // Input that arrived after the caller's poll still has its interrupt
    // pending, so this check-then-wfi cannot miss a wake-up.
    if !input_ready() {
        unsafe { asm!("wfi") };
    }
    // Timer off (and its interrupt with it) until the next deadline.
    unsafe { asm!("msr cntp_ctl_el0, {}", "isb", in(reg) 0u64) };
}
