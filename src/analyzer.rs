use regex::Regex;
use std::sync::LazyLock;

/// Result of static analysis
#[derive(Debug)]
pub struct AnalysisError {
    pub line: usize,
    pub instruction: String,
    pub message: String,
}

/// Forbidden instruction mnemonics (§4.2)
static FORBIDDEN_INSTRUCTIONS: &[&str] = &[
    "svc", "hvc", "smc", "eret", "brk", "hlt", "dcps1", "dcps2", "dcps3", "mrs", "msr", "sys",
    "sysl", "dc", "ic", "at", "tlbi",
];

/// Forbidden indirect branch mnemonics (§4.3)
static FORBIDDEN_INDIRECT_BRANCHES: &[&str] = &["br", "blr"];

/// Forbidden assembler directives (§4.3)
static FORBIDDEN_DIRECTIVES: &[&str] = &[".include", ".incbin", ".macro", ".endmacro"];

/// Forbidden data directives in .text section (§4.3)
static FORBIDDEN_DATA_IN_TEXT: &[&str] = &[".byte", ".word", ".long", ".quad"];

static LABEL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([a-zA-Z_][a-zA-Z0-9_]*):").unwrap());

/// Strip comments from a line: //, ;, and /* ... */
/// Note: block comments spanning multiple lines are handled by pre-processing the entire source.
fn strip_comments(source: &str) -> String {
    // First remove block comments
    let mut result = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block = false;
    while let Some(c) = chars.next() {
        if in_block {
            if c == '*' {
                if chars.peek() == Some(&'/') {
                    chars.next();
                    in_block = false;
                }
            }
            // Preserve newlines even inside block comments so line numbers stay correct
            if c == '\n' {
                result.push('\n');
            }
        } else if c == '/' {
            match chars.peek() {
                Some(&'/') => {
                    // Line comment — skip to end of line
                    for cc in chars.by_ref() {
                        if cc == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                }
                Some(&'*') => {
                    chars.next();
                    in_block = true;
                }
                _ => result.push(c),
            }
        } else if c == ';' {
            // Line comment — skip to end of line
            for cc in chars.by_ref() {
                if cc == '\n' {
                    result.push('\n');
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Track which section we're in
#[derive(Debug, Clone, Copy, PartialEq)]
enum Section {
    Text,
    Data,
    Rodata,
    Other,
}

/// Perform static analysis on assembly source code.
/// Returns Ok(()) if the code passes, or Err with details about the first violation found.
pub fn analyze(source: &str) -> Result<(), AnalysisError> {
    let cleaned = strip_comments(source);
    let mut current_section = Section::Text; // default section is .text
    let mut label_count: usize = 0;

    for (line_idx, line) in cleaned.lines().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Track section changes
        if let Some(section) = detect_section(trimmed) {
            current_section = section;
            continue;
        }

        // Count labels
        if LABEL_RE.is_match(trimmed) {
            label_count += 1;
            if label_count > 1000 {
                return Err(AnalysisError {
                    line: line_num,
                    instruction: trimmed.to_string(),
                    message: "Source exceeds maximum of 1,000 labels".to_string(),
                });
            }

            // Check reserved _harness_ namespace
            if let Some(caps) = LABEL_RE.captures(trimmed) {
                let label_name = &caps[1];
                if label_name.starts_with("_harness_") {
                    return Err(AnalysisError {
                        line: line_num,
                        instruction: label_name.to_string(),
                        message: format!(
                            "Label '{}' uses reserved '_harness_' prefix",
                            label_name
                        ),
                    });
                }
            }
        }

        // Extract the mnemonic (first token on the line, skipping labels)
        let instruction_part = if let Some(colon_pos) = trimmed.find(':') {
            trimmed[colon_pos + 1..].trim()
        } else {
            trimmed
        };

        if instruction_part.is_empty() {
            continue;
        }

        // Get the first token (mnemonic)
        let mnemonic = instruction_part
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();

        // Check forbidden directives (these start with .)
        for &dir in FORBIDDEN_DIRECTIVES {
            if mnemonic == dir {
                return Err(AnalysisError {
                    line: line_num,
                    instruction: dir.to_string(),
                    message: format!("Forbidden directive '{}' is not allowed", dir),
                });
            }
        }

        // Check forbidden data directives in .text section
        if current_section == Section::Text {
            for &dir in FORBIDDEN_DATA_IN_TEXT {
                if mnemonic == dir {
                    return Err(AnalysisError {
                        line: line_num,
                        instruction: dir.to_string(),
                        message: format!(
                            "Data directive '{}' is forbidden in .text section (allowed in .data/.rodata)",
                            dir
                        ),
                    });
                }
            }
        }

        // Skip directives (start with .) for instruction checks
        if mnemonic.starts_with('.') {
            continue;
        }

        // Check forbidden instructions
        for &instr in FORBIDDEN_INSTRUCTIONS {
            if mnemonic == instr {
                return Err(AnalysisError {
                    line: line_num,
                    instruction: instr.to_string(),
                    message: format!("Forbidden instruction '{}': {}", instr, reason_for(instr)),
                });
            }
        }

        // Check forbidden indirect branches
        for &instr in FORBIDDEN_INDIRECT_BRANCHES {
            if mnemonic == instr {
                return Err(AnalysisError {
                    line: line_num,
                    instruction: instr.to_string(),
                    message: format!(
                        "Indirect branch '{}' is forbidden. Use direct branches (b, bl) instead.",
                        instr
                    ),
                });
            }
        }
    }

    Ok(())
}

fn detect_section(trimmed: &str) -> Option<Section> {
    let lower = trimmed.to_lowercase();
    if lower.starts_with(".text") {
        Some(Section::Text)
    } else if lower.starts_with(".data") {
        Some(Section::Data)
    } else if lower.starts_with(".rodata") || lower.starts_with(".section") && lower.contains("rodata") {
        Some(Section::Rodata)
    } else if lower.starts_with(".section") {
        // Determine section type from .section directive
        if lower.contains("__text") || lower.contains(".text") {
            Some(Section::Text)
        } else if lower.contains("__data") || lower.contains(".data") {
            Some(Section::Data)
        } else if lower.contains("__const") || lower.contains("rodata") {
            Some(Section::Rodata)
        } else {
            Some(Section::Other)
        }
    } else {
        None
    }
}

fn reason_for(instr: &str) -> &'static str {
    match instr {
        "svc" => "Supervisor call (syscall)",
        "hvc" => "Hypervisor call",
        "smc" => "Secure monitor call",
        "eret" => "Exception return",
        "brk" => "Software breakpoint",
        "hlt" => "Halt",
        "dcps1" | "dcps2" | "dcps3" => "Debug exception",
        "mrs" => "Read system register",
        "msr" => "Write system register",
        "sys" => "System instruction",
        "sysl" => "System instruction with result",
        "dc" => "Cache maintenance (privileged)",
        "ic" => "Instruction cache maintenance (privileged)",
        "at" => "Address translation (privileged)",
        "tlbi" => "TLB invalidate (privileged)",
        _ => "Forbidden instruction",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_basic() {
        let source = r#"
.global _user_entry
.align 2

_user_entry:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    add x0, x0, x1
    ldp x29, x30, [sp], #16
    ret
"#;
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_forbidden_svc() {
        let source = "    svc #0x80\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "svc");
        assert_eq!(err.line, 1);
    }

    #[test]
    fn test_forbidden_hvc() {
        let source = "    HVC #0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "hvc");
    }

    #[test]
    fn test_forbidden_smc() {
        let source = "    smc #0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "smc");
    }

    #[test]
    fn test_forbidden_eret() {
        let source = "    ERET\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "eret");
    }

    #[test]
    fn test_forbidden_brk() {
        let source = "    brk #0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "brk");
    }

    #[test]
    fn test_forbidden_hlt() {
        let source = "    hlt #0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "hlt");
    }

    #[test]
    fn test_forbidden_dcps1() {
        let source = "    dcps1\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "dcps1");
    }

    #[test]
    fn test_forbidden_mrs() {
        let source = "    mrs x0, nzcv\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "mrs");
    }

    #[test]
    fn test_forbidden_msr() {
        let source = "    msr nzcv, x0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "msr");
    }

    #[test]
    fn test_forbidden_sys() {
        let source = "    sys #1, c1, c0, #0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "sys");
    }

    #[test]
    fn test_forbidden_dc() {
        let source = "    dc civac, x0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "dc");
    }

    #[test]
    fn test_forbidden_tlbi() {
        let source = "    tlbi alle1\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "tlbi");
    }

    #[test]
    fn test_forbidden_indirect_branch_br() {
        let source = "    br x8\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "br");
    }

    #[test]
    fn test_forbidden_indirect_branch_blr() {
        let source = "    blr x8\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "blr");
    }

    #[test]
    fn test_forbidden_include() {
        let source = "    .include \"other.s\"\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, ".include");
    }

    #[test]
    fn test_forbidden_incbin() {
        let source = "    .incbin \"data.bin\"\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, ".incbin");
    }

    #[test]
    fn test_forbidden_macro() {
        let source = "    .macro mymacro\n    nop\n    .endmacro\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, ".macro");
    }

    #[test]
    fn test_forbidden_byte_in_text() {
        let source = ".text\n    .byte 0xd4\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, ".byte");
    }

    #[test]
    fn test_allowed_byte_in_data() {
        let source = ".data\n    .byte 0xd4\n";
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_allowed_word_in_rodata() {
        let source = ".rodata\n    .word 42\n";
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_forbidden_word_in_text() {
        let source = ".text\n    .word 0xd4000001\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, ".word");
    }

    #[test]
    fn test_reserved_harness_label() {
        let source = "_harness_my_func:\n    ret\n";
        let err = analyze(source).unwrap_err();
        assert!(err.message.contains("_harness_"));
    }

    #[test]
    fn test_label_not_false_positive_msr() {
        // Label containing "msr" should not trigger
        let source = "_my_msr_counter:\n    add x0, x0, #1\n    ret\n";
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_label_count_limit() {
        let mut source = String::new();
        for i in 0..1001 {
            source.push_str(&format!("_label{}:\n    nop\n", i));
        }
        let err = analyze(&source).unwrap_err();
        assert!(err.message.contains("1,000 labels"));
    }

    #[test]
    fn test_comments_stripped() {
        let source = r#"
// This is a comment with svc in it
_user_entry:  ; svc should not trigger
    add x0, x0, #1  // mrs not an issue here
    /* block comment with brk inside */
    ret
"#;
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_allowed_direct_branches() {
        let source = r#"
_user_entry:
    b _loop
    bl _helper
    cbz x0, _done
    cbnz x1, _loop
    tbz x2, #0, _done
    tbnz x3, #1, _loop
_loop:
    nop
_done:
    ret
_helper:
    ret
"#;
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_allowed_conditional_branches() {
        let source = r#"
_user_entry:
    cmp x0, #0
    b.eq _done
    b.ne _loop
    b.lt _done
    b.gt _loop
_loop:
    nop
_done:
    ret
"#;
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_allowed_simd_instructions() {
        let source = r#"
_user_entry:
    fmov d0, x0
    fadd d0, d0, d1
    fmul d2, d0, d1
    ret
"#;
        assert!(analyze(source).is_ok());
    }

    #[test]
    fn test_correct_line_number() {
        let source = "    nop\n    nop\n    svc #0\n    nop\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.line, 3);
    }

    #[test]
    fn test_case_insensitive_detection() {
        let source = "    SVC #0x80\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "svc");
    }

    #[test]
    fn test_instruction_after_label_on_same_line() {
        let source = "_entry: svc #0x80\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "svc");
    }

    #[test]
    fn test_sysl_forbidden() {
        let source = "    sysl x0, #1, c1, c0, #0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "sysl");
    }

    #[test]
    fn test_at_forbidden() {
        let source = "    at s1e1r, x0\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "at");
    }

    #[test]
    fn test_ic_forbidden() {
        let source = "    ic iallu\n";
        let err = analyze(source).unwrap_err();
        assert_eq!(err.instruction, "ic");
    }
}