use std::collections::HashMap;

/// Generate the harness assembly source for a given request.
///
/// The harness:
/// 1. Sets up a user stack (64 KB)
/// 2. Loads input registers x0–x7
/// 3. Runs warmup iterations (10% if iterations > 10, else 0)
/// 4. Runs timed iterations, recording per-iteration tick counts
/// 5. Computes summary statistics (min, max, mean, median, stddev)
/// 6. Outputs results in wire format
/// 7. Exits cleanly
pub fn generate_harness(
    entrypoint: &str,
    inputs: &HashMap<String, i64>,
    iterations: u64,
) -> String {
    let warmup_count = if iterations > 10 {
        std::cmp::max(1, iterations / 10)
    } else {
        0
    };
    let measured_count = iterations - warmup_count;

    let load_inputs = generate_input_loads(inputs);

    format!(
        r#"// Harness for ARM64 Sandbox API
.global _main
.align 2

.equ USER_STACK_SIZE, 65536
.equ TIMER_FREQ, 24000000

// ============================================================
// Data section
// ============================================================
.data

{input_data}

_str_harness_prefix:
    .asciz "HARNESS:rv="
_str_semi_n:
    .asciz ";n="
_str_semi_freq:
    .asciz ";freq=24000000;total="
_str_semi_mean:
    .asciz ";mean="
_str_semi_median:
    .asciz ";median="
_str_semi_min:
    .asciz ";min="
_str_semi_max:
    .asciz ";max="
_str_semi_stddev:
    .asciz ";stddev="
_str_newline:
    .asciz "\n"

.bss
.align 4
_user_stack:
    .space USER_STACK_SIZE
_user_stack_top:
    .space 8

.align 4
_timing_buffer:
    .space {timing_buffer_size}

.align 4
_num_buf:
    .space 32

.text
.align 2

// ============================================================
// Main entry point
// ============================================================
_main:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x19, x20, [sp, #-16]!
    stp x21, x22, [sp, #-16]!
    stp x23, x24, [sp, #-16]!
    stp x25, x26, [sp, #-16]!
    stp x27, x28, [sp, #-16]!

    // Save the real stack pointer for syscalls later
    mov x19, sp

    // Set up user stack
    adrp x9, _user_stack_top@PAGE
    add x9, x9, _user_stack_top@PAGEOFF
    mov sp, x9

    // ---- Warm-up phase ----
    {load_warmup_count}
    cbz x21, _harness_warmup_done

_harness_warmup_loop:
{load_inputs_indented}
    bl {entrypoint}
    sub x21, x21, #1
    cbnz x21, _harness_warmup_loop

_harness_warmup_done:

    // ---- Timed phase ----
    {load_measured_count}
    cbz x21, _harness_bench_done
    adrp x23, _timing_buffer@PAGE
    add x23, x23, _timing_buffer@PAGEOFF

_harness_bench_loop:
{load_inputs_indented}
    mrs x20, cntvct_el0
    bl {entrypoint}
    mrs x22, cntvct_el0
    sub x22, x22, x20
    str x22, [x23], #8
    sub x21, x21, #1
    cbnz x21, _harness_bench_loop

_harness_bench_done:
    // x0 holds the return value from the final measured iteration
    mov x24, x0

    // Restore real stack for syscalls
    mov sp, x19

    // ---- Compute statistics ----
    adrp x23, _timing_buffer@PAGE
    add x23, x23, _timing_buffer@PAGEOFF
    {load_measured_count_x21}

    cbz x21, _harness_no_stats

    // Initialize: total, min, max from first element
    ldr x25, [x23]
    mov x26, x25
    mov x27, x25
    mov x28, #1
    cmp x28, x21
    b.ge _harness_stats_done

_harness_stats_loop:
    ldr x9, [x23, x28, lsl #3]
    add x25, x25, x9
    cmp x9, x26
    csel x26, x9, x26, lo
    cmp x9, x27
    csel x27, x9, x27, hi
    add x28, x28, #1
    cmp x28, x21
    b.lt _harness_stats_loop

_harness_stats_done:
    // x25=total, x26=min, x27=max
    udiv x10, x25, x21            // x10 = mean

    // Insertion sort for median computation
    mov x9, #1
_harness_sort_outer:
    cmp x9, x21
    b.ge _harness_sort_done
    ldr x10, [x23, x9, lsl #3]
    sub x11, x9, #1
_harness_sort_inner:
    tbnz x11, #63, _harness_sort_insert
    ldr x12, [x23, x11, lsl #3]
    cmp x12, x10
    b.ls _harness_sort_insert
    add x13, x11, #1
    str x12, [x23, x13, lsl #3]
    sub x11, x11, #1
    b _harness_sort_inner
_harness_sort_insert:
    add x13, x11, #1
    str x10, [x23, x13, lsl #3]
    add x9, x9, #1
    b _harness_sort_outer
_harness_sort_done:

    // Median: buf[n/2]
    lsr x9, x21, #1
    ldr x11, [x23, x9, lsl #3]    // x11 = median

    // Recompute mean (total unchanged by sort)
    udiv x10, x25, x21            // x10 = mean

    // Compute stddev: isqrt(sum((xi - mean)^2) / n)
    mov x12, #0
    mov x9, #0
_harness_stddev_loop:
    cmp x9, x21
    b.ge _harness_stddev_done
    ldr x13, [x23, x9, lsl #3]
    subs x14, x13, x10
    mul x14, x14, x14
    add x12, x12, x14
    add x9, x9, #1
    b _harness_stddev_loop
_harness_stddev_done:

    // variance = sum_sq_diff / n
    udiv x12, x12, x21
    // stddev = isqrt(variance) via Newton's method
    cbz x12, _harness_stddev_zero
    mov x13, x12
    lsr x13, x13, #1
    cbz x13, _harness_sqrt_one
_harness_sqrt_loop:
    udiv x14, x12, x13
    add x14, x14, x13
    lsr x14, x14, #1
    cmp x14, x13
    b.ge _harness_sqrt_done_calc
    mov x13, x14
    b _harness_sqrt_loop
_harness_sqrt_one:
    mov x13, #1
    b _harness_sqrt_done_calc
_harness_stddev_zero:
    mov x13, #0
_harness_sqrt_done_calc:
    // x13 = stddev in ticks

    // ---- Output wire format ----
    // Push stats to stack. Layout after all four stp instructions:
    //   sp+ 0: x27 (max)     sp+ 8: x13 (stddev)
    //   sp+16: x11 (median)  sp+24: x26 (min)
    //   sp+32: x25 (total)   sp+40: x10 (mean)
    //   sp+48: x24 (rv)      sp+56: x21 (n)
    stp x24, x21, [sp, #-16]!
    stp x25, x10, [sp, #-16]!
    stp x11, x26, [sp, #-16]!
    stp x27, x13, [sp, #-16]!

    // "HARNESS:rv="
    adrp x0, _str_harness_prefix@PAGE
    add x0, x0, _str_harness_prefix@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #48]             // rv
    bl _harness_print_i64_internal

    adrp x0, _str_semi_n@PAGE
    add x0, x0, _str_semi_n@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #56]             // n
    bl _harness_print_u64_internal

    adrp x0, _str_semi_freq@PAGE
    add x0, x0, _str_semi_freq@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #32]             // total
    bl _harness_print_u64_internal

    adrp x0, _str_semi_mean@PAGE
    add x0, x0, _str_semi_mean@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #40]             // mean
    bl _harness_print_u64_internal

    adrp x0, _str_semi_median@PAGE
    add x0, x0, _str_semi_median@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #16]             // median
    bl _harness_print_u64_internal

    adrp x0, _str_semi_min@PAGE
    add x0, x0, _str_semi_min@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #24]             // min
    bl _harness_print_u64_internal

    adrp x0, _str_semi_max@PAGE
    add x0, x0, _str_semi_max@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #0]              // max
    bl _harness_print_u64_internal

    adrp x0, _str_semi_stddev@PAGE
    add x0, x0, _str_semi_stddev@PAGEOFF
    bl _harness_print_str

    ldr x0, [sp, #8]              // stddev
    bl _harness_print_u64_internal

    adrp x0, _str_newline@PAGE
    add x0, x0, _str_newline@PAGEOFF
    bl _harness_print_str

    // Clean up stats stack frame
    add sp, sp, #64

    b _harness_exit

_harness_no_stats:
    // Zero iterations: emit minimal wire format
    adrp x0, _str_harness_prefix@PAGE
    add x0, x0, _str_harness_prefix@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_i64_internal
    adrp x0, _str_semi_n@PAGE
    add x0, x0, _str_semi_n@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_u64_internal
    adrp x0, _str_semi_freq@PAGE
    add x0, x0, _str_semi_freq@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_u64_internal
    adrp x0, _str_semi_mean@PAGE
    add x0, x0, _str_semi_mean@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_u64_internal
    adrp x0, _str_semi_median@PAGE
    add x0, x0, _str_semi_median@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_u64_internal
    adrp x0, _str_semi_min@PAGE
    add x0, x0, _str_semi_min@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_u64_internal
    adrp x0, _str_semi_max@PAGE
    add x0, x0, _str_semi_max@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_u64_internal
    adrp x0, _str_semi_stddev@PAGE
    add x0, x0, _str_semi_stddev@PAGEOFF
    bl _harness_print_str
    mov x0, #0
    bl _harness_print_u64_internal
    adrp x0, _str_newline@PAGE
    add x0, x0, _str_newline@PAGEOFF
    bl _harness_print_str

_harness_exit:
    mov x0, #0
    mov x16, #1
    svc #0x80

// ============================================================
// Internal print helpers (used by harness only)
// ============================================================

// Print null-terminated string at address in x0
_harness_print_str:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    mov x1, x0
    mov x2, #0
_harness_strlen:
    ldrb w3, [x1, x2]
    cbz w3, _harness_strlen_done
    add x2, x2, #1
    b _harness_strlen
_harness_strlen_done:
    mov x0, #1
    mov x16, #4
    svc #0x80
    ldp x29, x30, [sp], #16
    ret

// Print unsigned 64-bit value in x0 as decimal
_harness_print_u64_internal:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x19, x20, [sp, #-16]!

    mov x19, x0
    adrp x20, _num_buf@PAGE
    add x20, x20, _num_buf@PAGEOFF
    add x20, x20, #30
    mov w1, #0
    strb w1, [x20]

    cbz x19, _harness_u64_zero

_harness_u64_loop:
    cbz x19, _harness_u64_print
    mov x1, #10
    udiv x2, x19, x1
    msub x3, x2, x1, x19
    add w3, w3, #'0'
    sub x20, x20, #1
    strb w3, [x20]
    mov x19, x2
    b _harness_u64_loop

_harness_u64_zero:
    sub x20, x20, #1
    mov w1, #'0'
    strb w1, [x20]

_harness_u64_print:
    mov x0, x20
    ldp x19, x20, [sp], #16
    ldp x29, x30, [sp], #16
    b _harness_print_str

// Print signed 64-bit value in x0 as decimal
_harness_print_i64_internal:
    stp x29, x30, [sp, #-16]!
    mov x29, sp

    cmp x0, #0
    b.ge _harness_i64_positive

    stp x0, xzr, [sp, #-16]!
    mov w0, #'-'
    bl _harness_write_char
    ldp x0, xzr, [sp], #16
    neg x0, x0

_harness_i64_positive:
    ldp x29, x30, [sp], #16
    b _harness_print_u64_internal

// Write single character from low byte of w0
_harness_write_char:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    sub sp, sp, #16
    strb w0, [sp]
    mov x0, #1
    mov x1, sp
    mov x2, #1
    mov x16, #4
    svc #0x80
    add sp, sp, #16
    ldp x29, x30, [sp], #16
    ret

// ============================================================
// User-callable print helpers (preserve x1-x30)
// ============================================================

.global _harness_print_u64
_harness_print_u64:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x1, x2, [sp, #-16]!
    stp x3, x4, [sp, #-16]!
    stp x5, x6, [sp, #-16]!
    stp x7, x8, [sp, #-16]!
    stp x9, x10, [sp, #-16]!
    stp x11, x12, [sp, #-16]!
    stp x13, x14, [sp, #-16]!
    stp x15, x16, [sp, #-16]!
    stp x17, x18, [sp, #-16]!

    bl _harness_print_u64_internal
    adrp x0, _str_newline@PAGE
    add x0, x0, _str_newline@PAGEOFF
    bl _harness_print_str

    ldp x17, x18, [sp], #16
    ldp x15, x16, [sp], #16
    ldp x13, x14, [sp], #16
    ldp x11, x12, [sp], #16
    ldp x9, x10, [sp], #16
    ldp x7, x8, [sp], #16
    ldp x5, x6, [sp], #16
    ldp x3, x4, [sp], #16
    ldp x1, x2, [sp], #16
    ldp x29, x30, [sp], #16
    ret

.global _harness_print_i64
_harness_print_i64:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x1, x2, [sp, #-16]!
    stp x3, x4, [sp, #-16]!
    stp x5, x6, [sp, #-16]!
    stp x7, x8, [sp, #-16]!
    stp x9, x10, [sp, #-16]!
    stp x11, x12, [sp, #-16]!
    stp x13, x14, [sp, #-16]!
    stp x15, x16, [sp, #-16]!
    stp x17, x18, [sp, #-16]!

    bl _harness_print_i64_internal
    adrp x0, _str_newline@PAGE
    add x0, x0, _str_newline@PAGEOFF
    bl _harness_print_str

    ldp x17, x18, [sp], #16
    ldp x15, x16, [sp], #16
    ldp x13, x14, [sp], #16
    ldp x11, x12, [sp], #16
    ldp x9, x10, [sp], #16
    ldp x7, x8, [sp], #16
    ldp x5, x6, [sp], #16
    ldp x3, x4, [sp], #16
    ldp x1, x2, [sp], #16
    ldp x29, x30, [sp], #16
    ret

.global _harness_print_hex
_harness_print_hex:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x1, x2, [sp, #-16]!
    stp x3, x4, [sp, #-16]!
    stp x5, x6, [sp, #-16]!
    stp x7, x8, [sp, #-16]!
    stp x9, x10, [sp, #-16]!
    stp x11, x12, [sp, #-16]!
    stp x13, x14, [sp, #-16]!
    stp x15, x16, [sp, #-16]!
    stp x17, x18, [sp, #-16]!
    stp x19, x20, [sp, #-16]!

    mov x19, x0

    mov w0, #'0'
    bl _harness_write_char
    mov w0, #'x'
    bl _harness_write_char

    mov x20, #60
    mov w9, #0
_harness_hex_loop:
    lsr x0, x19, x20
    and x0, x0, #0xf
    cbnz x0, _harness_hex_nonzero
    cbnz w9, _harness_hex_nonzero
    cbz x20, _harness_hex_nonzero
    sub x20, x20, #4
    b _harness_hex_loop
_harness_hex_nonzero:
    mov w9, #1
    cmp x0, #10
    b.lt _harness_hex_digit
    add w0, w0, #('a' - 10)
    bl _harness_write_char
    b _harness_hex_next
_harness_hex_digit:
    add w0, w0, #'0'
    bl _harness_write_char
_harness_hex_next:
    cbz x20, _harness_hex_done
    sub x20, x20, #4
    b _harness_hex_loop
_harness_hex_done:
    adrp x0, _str_newline@PAGE
    add x0, x0, _str_newline@PAGEOFF
    bl _harness_print_str

    ldp x19, x20, [sp], #16
    ldp x17, x18, [sp], #16
    ldp x15, x16, [sp], #16
    ldp x13, x14, [sp], #16
    ldp x11, x12, [sp], #16
    ldp x9, x10, [sp], #16
    ldp x7, x8, [sp], #16
    ldp x5, x6, [sp], #16
    ldp x3, x4, [sp], #16
    ldp x1, x2, [sp], #16
    ldp x29, x30, [sp], #16
    ret

.global _harness_print_char
_harness_print_char:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x1, x2, [sp, #-16]!
    stp x3, x4, [sp, #-16]!
    stp x15, x16, [sp, #-16]!
    stp x17, x18, [sp, #-16]!

    and w0, w0, #0xff
    bl _harness_write_char

    ldp x17, x18, [sp], #16
    ldp x15, x16, [sp], #16
    ldp x3, x4, [sp], #16
    ldp x1, x2, [sp], #16
    ldp x29, x30, [sp], #16
    ret

.global _harness_print_newline
_harness_print_newline:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x1, x2, [sp, #-16]!
    stp x15, x16, [sp, #-16]!
    stp x17, x18, [sp, #-16]!

    adrp x0, _str_newline@PAGE
    add x0, x0, _str_newline@PAGEOFF
    bl _harness_print_str

    ldp x17, x18, [sp], #16
    ldp x15, x16, [sp], #16
    ldp x1, x2, [sp], #16
    ldp x29, x30, [sp], #16
    ret
"#,
        input_data = generate_input_data(inputs),
        load_inputs_indented = indent(&load_inputs, "    "),
        entrypoint = entrypoint,
        load_warmup_count = load_u64_to_reg("x21", warmup_count),
        load_measured_count = load_u64_to_reg("x21", measured_count),
        load_measured_count_x21 = load_u64_to_reg("x21", measured_count),
        timing_buffer_size = measured_count.max(1) * 8,
    )
}

/// Generate ARM64 instructions to load a u64 value into a register.
/// Uses movz/movk pairs for values that don't fit in a single mov immediate.
fn load_u64_to_reg(reg: &str, value: u64) -> String {
    if value <= 0xFFFF {
        format!("mov {}, #{}", reg, value)
    } else if value <= 0xFFFF_FFFF {
        let lo = value & 0xFFFF;
        let hi = (value >> 16) & 0xFFFF;
        format!(
            "movz {reg}, #0x{lo:x}\n    movk {reg}, #0x{hi:x}, lsl #16",
            reg = reg,
            lo = lo,
            hi = hi
        )
    } else {
        let w0 = value & 0xFFFF;
        let w1 = (value >> 16) & 0xFFFF;
        let w2 = (value >> 32) & 0xFFFF;
        let w3 = (value >> 48) & 0xFFFF;
        let mut parts = vec![format!("movz {}, #0x{:x}", reg, w0)];
        if w1 != 0 {
            parts.push(format!("movk {}, #0x{:x}, lsl #16", reg, w1));
        }
        if w2 != 0 {
            parts.push(format!("movk {}, #0x{:x}, lsl #32", reg, w2));
        }
        if w3 != 0 {
            parts.push(format!("movk {}, #0x{:x}, lsl #48", reg, w3));
        }
        parts.join("\n    ")
    }
}

/// Generate .data section entries for input values
fn generate_input_data(inputs: &HashMap<String, i64>) -> String {
    let mut lines = Vec::new();
    for i in 0..8 {
        let reg = format!("x{}", i);
        let val = inputs.get(&reg).copied().unwrap_or(0);
        lines.push(format!("_harness_input_{}: .quad {}", reg, val));
    }
    lines.join("\n")
}

/// Generate instructions to load input registers from data section
fn generate_input_loads(inputs: &HashMap<String, i64>) -> String {
    let mut lines = Vec::new();
    for i in 0..8 {
        let reg = format!("x{}", i);
        let val = inputs.get(&reg).copied().unwrap_or(0);
        if val != 0 || inputs.contains_key(&reg) {
            lines.push(format!(
                "adrp x9, _harness_input_{reg}@PAGE\n    add x9, x9, _harness_input_{reg}@PAGEOFF\n    ldr {reg}, [x9]",
                reg = reg
            ));
        } else {
            lines.push(format!("mov {}, #0", reg));
        }
    }
    lines.join("\n")
}

fn indent(s: &str, prefix: &str) -> String {
    s.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{}{}", prefix, line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_harness_basic() {
        let inputs = HashMap::from([("x0".to_string(), 100i64)]);
        let harness = generate_harness("_user_entry", &inputs, 1000);
        assert!(harness.contains(".global _main"));
        assert!(harness.contains("bl _user_entry"));
        assert!(harness.contains("mrs x20, cntvct_el0"));
        // warmup = 100, measured = 900 — both fit in 16-bit mov
        assert!(harness.contains("mov x21, #100"));
        assert!(harness.contains("mov x21, #900"));
    }

    #[test]
    fn test_generate_harness_no_warmup() {
        let inputs = HashMap::new();
        let harness = generate_harness("_user_entry", &inputs, 5);
        // iterations <= 10 means warmup = 0, measured = 5
        assert!(harness.contains("mov x21, #0"));
    }

    #[test]
    fn test_generate_harness_custom_entrypoint() {
        let inputs = HashMap::new();
        let harness = generate_harness("_my_func", &inputs, 1);
        assert!(harness.contains("bl _my_func"));
    }

    #[test]
    fn test_generate_input_data() {
        let inputs = HashMap::from([("x0".to_string(), 42i64), ("x3".to_string(), -1i64)]);
        let data = generate_input_data(&inputs);
        assert!(data.contains("_harness_input_x0: .quad 42"));
        assert!(data.contains("_harness_input_x3: .quad -1"));
    }

    #[test]
    fn test_harness_contains_no_narrative_comments() {
        let inputs = HashMap::from([("x0".to_string(), 1i64)]);
        let harness = generate_harness("_user_entry", &inputs, 100);
        // Ensure no debug/narrative text leaked into output
        assert!(!harness.contains("Correction"));
        assert!(!harness.contains("Let me"));
        assert!(!harness.contains("re-check"));
        assert!(!harness.contains("Actually"));
        assert!(!harness.contains("Wait,"));
    }

    #[test]
    fn test_generate_harness_large_iterations() {
        let inputs = HashMap::new();
        let harness = generate_harness("_user_entry", &inputs, 1_000_000);
        // warmup = 100000, measured = 900000 — need movz/movk
        assert!(harness.contains("movz x21"));
        assert!(harness.contains("movk x21"));
    }
}