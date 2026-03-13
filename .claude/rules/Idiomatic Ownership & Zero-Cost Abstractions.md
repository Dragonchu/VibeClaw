---
description: Enforces idiomatic Rust ownership patterns, borrowing, and prohibits unnecessary cloning or unsafe blocks.
paths:
  - '**/*.rs'
---

# Rule: Idiomatic Ownership & Zero-Cost Abstractions

## Core Principle

Write high-performance, idiomatic Rust. Prefer borrowing over cloning and stack allocation over heap allocation whenever possible.

## Mandatory Constraints

1. **No Excessive Cloning:** Do not use `.clone()` to bypass borrow checker issues. If a lifetime issue arises, solve it with proper references or architectural changes.
2. **Unsafe is Forbidden:** Never use `unsafe` blocks unless there is a documented, unavoidable performance reason or FFI requirement.
3. **Smart Pointer Selection:** Use `Box`, `Rc`, `Arc`, or `RefCell` only when necessary. Prefer standard references `&T` or `&mut T` for function arguments.
4. **Zero-Copy:** Use slices (`&[T]`, `&str`) instead of owned collections (`Vec<T>`, `String`) in function signatures where ownership isn't required.

## Behavioral Expectations

- If you find yourself needing to `clone()` to make the code compile, stop and ask: "Is there a better way to structure this data flow?"
