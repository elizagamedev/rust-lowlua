lowlua
======

Low-level Lua bindings for Rust. This is a mostly thin wrapper around
[lua53-sys](https://github.com/mathewv/rust-lua53-sys), and while the
API itself is safe, caution must be exercised when interacting with
Lua scripts to ensure sane behavior as in the original C API.

This library is not really ready to be used with any other projects
at this time.