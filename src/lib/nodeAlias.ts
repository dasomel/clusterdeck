// Mirrors Rust `NODE_ALIAS_DOMAIN` (services/validate.rs). `.internal`, not `.local`: negative
// lookups under mDNS `.local` stall ~5 s on macOS (issue #119, ADR-0011 D6 amendment).
export const NODE_ALIAS_DOMAIN = 'clusterdeck.internal';
