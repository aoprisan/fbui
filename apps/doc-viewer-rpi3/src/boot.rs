//! Reset vector: park the secondary cores, drop from EL3/EL2 to EL1, enable
//! the FPU (Rust uses it for every `f32`), set the stack, zero `.bss`, and
//! call `kernel_main`.

core::arch::global_asm!(
    r#"
.section .text.boot
.global _start
_start:
    // Only core 0 runs; the others sleep forever. (`wfi`, not `wfe`: with
    // no interrupt routed to them it never wakes, where `wfe` may spin.)
    mrs     x1, mpidr_el1
    and     x1, x1, #3
    cbz     x1, 1f
0:  wfi
    b       0b

1:  mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #3
    b.ne    2f
    // EL3 -> EL2: non-secure, EL2 is AArch64.
    mov     x0, #0x5b1
    msr     scr_el3, x0
    mov     x0, #0x3c9
    msr     spsr_el3, x0
    adr     x0, 2f
    msr     elr_el3, x0
    eret

2:  mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #2
    b.ne    3f
    // EL2 -> EL1: EL1 is AArch64, no FP/SIMD traps, EL1 may read the
    // physical counter and program the physical timer.
    mov     x0, #(1 << 31)
    msr     hcr_el2, x0
    mov     x0, #0x33ff
    msr     cptr_el2, x0
    msr     hstr_el2, xzr
    mrs     x0, cnthctl_el2
    orr     x0, x0, #3
    msr     cnthctl_el2, x0
    msr     cntvoff_el2, xzr
    mov     x0, #0x3c5
    msr     spsr_el2, x0
    adr     x0, 3f
    msr     elr_el2, x0
    eret

3:  // EL1: FP/SIMD on (CPACR_EL1.FPEN = 0b11).
    mov     x0, #(3 << 20)
    msr     cpacr_el1, x0
    isb
    ldr     x0, =__stack_top
    mov     sp, x0
    ldr     x1, =__bss_start
    ldr     x2, =__bss_end
4:  cmp     x1, x2
    b.hs    5f
    str     xzr, [x1], #8
    b       4b
5:  bl      kernel_main
6:  wfi
    b       6b
"#
);
