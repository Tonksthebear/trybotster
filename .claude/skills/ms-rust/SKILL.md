---
name: ms-rust
description: ALWAYS use this skill BEFORE writing or modifying ANY Rust code (.rs files), even for simple Hello World programs. Enforces Microsoft Rust coding guidelines, applies M-CANONICAL-DOCS documentation, adds compliance comments, and validates against the guidelines. This skill is MANDATORY for all Rust development.
---

# Rust Development

This skill automatically enforces Rust coding standards and best practices when creating or modifying Rust code.

## Instructions

**CRITICAL**: This skill MUST be invoked for ANY Rust code operation, including:

- Creating new .rs files (even simple examples like Hello World)
- Modifying existing .rs files (any change, no matter how small)
- Reviewing Rust code
- Refactoring Rust code

**Process**:

1. Read the relevant guideline sections from `./resources/` (see table below)
2. Before writing/modifying ANY Rust code, ensure edits are conformant to the guidelines
3. Apply proper M-CANONICAL-DOCS documentation format
4. Add compliance comments
5. Comments must ALWAYS be written in American English, unless the user explicitly requests a different language
6. If the file is fully compliant, add a comment: `// Rust guideline compliant {date}` where {date} is the guideline date/version

**No exceptions**: Even for trivial code like "Hello World", guidelines must be followed.

---

## Guideline Reference

| Section | File | When to Read |
|---------|------|--------------|
| Overview | [00-overview.md](./resources/00-overview.md) | Always - quick intro |
| AI Guidelines | [01-ai-guidelines.md](./resources/01-ai-guidelines.md) | Always - AI-friendly code patterns |
| Application Guidelines | [02-application-guidelines.md](./resources/02-application-guidelines.md) | Building applications |
| Documentation | [03-documentation.md](./resources/03-documentation.md) | Writing docs, module docs |
| FFI Guidelines | [04-ffi-guidelines.md](./resources/04-ffi-guidelines.md) | C interop, unsafe FFI |
| Performance | [05-performance-guidelines.md](./resources/05-performance-guidelines.md) | Optimization, allocators |
| Safety | [06-safety-guidelines.md](./resources/06-safety-guidelines.md) | Unsafe code, memory safety |
| Universal | [07-universal-guidelines.md](./resources/07-universal-guidelines.md) | **Always** - core patterns |
| Libraries: Building | [08-libraries-building.md](./resources/08-libraries-building.md) | Creating crates/libraries |
| Libraries: Interop | [09-libraries-interoperability.md](./resources/09-libraries-interoperability.md) | Cross-crate compatibility |
| Libraries: Resilience | [10-libraries-resilience.md](./resources/10-libraries-resilience.md) | Error handling, robustness |
| Libraries: UX | [11-libraries-ux.md](./resources/11-libraries-ux.md) | API design, user experience |

**Key sections to always read**: `01-ai-guidelines.md`, `07-universal-guidelines.md`

---
