/// Internal HIR argument used to carry pre-v5 output transparency without
/// exposing it as a modern source-language parameter.
pub const LEGACY_TRANSPARENCY_ARG: &str = "$legacy_transp";

/// Internal marker for an optional built-in parameter skipped by a named call.
/// Lowering retains the empty slot so runtime built-ins have one positional
/// binding model without confusing omission with an explicit `na` argument.
pub const OMITTED_BUILTIN_ARG: &str = "$omitted_builtin_arg";
