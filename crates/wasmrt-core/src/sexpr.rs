//! `sexpr` — the S-expression lexer/parser front-end for `.wat`/`.wast` text.
//!
//! Ported at **T6** (`cmem/roadmap.md`). Depth-capped against paren bombs; must fail loud on
//! malformed input, never hang (wazmrt's lone-`;` non-advance hang is the cautionary tale).
