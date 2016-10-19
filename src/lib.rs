#![feature(try_from)]

extern crate lua53_sys as ffi;
extern crate libc;
mod state;

use std::{result, io, fmt, error};
use std::string::FromUtf8Error;

pub use state::*;

/// Type for native functions.
///
/// In order to communicate properly with Lua, a native function must use the following protocol,
/// which defines the way parameters and results are passed: a native function receives its
/// arguments from Lua in its stack in direct order (the first argument is pushed first).
/// So, when the function starts, `State::get_top()` returns the number of arguments received by the
/// function. The first argument (if any) is at index 1 and its last argument is at index
/// `State::get_top()`. To return values to Lua, a native function just pushes them onto the stack,
/// in direct order (the first result is pushed first), and returns the number of results.
/// Any other value in the stack below the results will be properly discarded by Lua.
/// Like a Lua function, a native function called by Lua can also return many results.
pub type NativeFunction = fn(&mut State) -> Result<u32>;

/// Enum of native Lua types.
#[derive(Debug)]
#[derive(Eq)]
#[derive(PartialEq)]
pub enum LuaType {
    Nil,
    Boolean,
    Number,
    String,
    Function,
    LightUserdata,
    Userdata,
    Thread,
    Table,
}

/// Enum of Lua arithmetic operations.
pub enum LuaOperator {
    /// Addition (+)
    Add,
    /// Subtraction (-)
    Sub,
    /// Multiplication (*)
    Mul,
    /// Float division (/)
    Div,
    /// Floor division (//)
    IDiv,
    /// Modulo (%)
    Mod,
    /// Exponentiation (^)
    Pow,
    /// Mathematical negation (unary -)
    Unm,
    /// Bitwise NOT (~)
    BNot,
    /// Bitwise AND (&)
    BAnd,
    /// Bitwise OR (|)
    BOr,
    /// Bitwise exclusive OR (~)
    BXor,
    /// Left shift (<<)
    Shl,
    /// Right shift (>>)
    Shr,
}

/// Used when calling Lua functions to specify the number of results (return values) desired to be
/// placed on the stack after the call. `MultiRet` is unbounded, but `Num` limits them to its value.
pub enum LuaCallResults {
    Num(u32),
    MultRet,
}

/// Used to specify the Lua indexing mode when using functions with multiple indexing modes.
#[derive(Copy)]
#[derive(Clone)]
pub enum LuaIndex {
    Stack(i32),
    Upvalue(u32),
    Registry,
}

impl LuaIndex {
    fn to_ffi(&self) -> libc::c_int {
        match *self {
            LuaIndex::Stack(val) => val,
            LuaIndex::Upvalue(val) => ffi::lua_upvalueindex(val as i32 + 1),
            LuaIndex::Registry => ffi::LUA_REGISTRYINDEX,
        }
    }
}

/// An interned Lua string. Guaranteed not to be garbage collected, as a reference to the string
/// is permanently stored in the registry.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct LuaString(usize);

/// A result which can return a Lua error.
pub type Result<T> = result::Result<T, Error>;

/// Describes a Lua error.
#[derive(Debug)]
pub enum Error {
    /// An IO error occurred.
    Io(io::Error),
    /// A UTF-8 conversion error occurred.
    Utf8(FromUtf8Error),
    /// An error occurred while converting types.
    Type,
    /// A syntax error occurred.
    Syntax(String),
    /// A runtime error occurred.
    Runtime(String, String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref err) => err.fmt(f),
            Error::Utf8(ref err) => err.fmt(f),
            Error::Type => write!(f, "An invalid Lua/native type conversion was attempted."),
            Error::Syntax(ref str) => write!(f, "{}", str),
            Error::Runtime(ref str, _) => write!(f, "{}", str),
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Io(ref err) => err.description(),
            Error::Utf8(ref err) => err.description(),
            Error::Type => "type conversion error",
            Error::Syntax(_) => "Lua syntax error",
            Error::Runtime(_, _) => "Lua runtime error",
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::Io(ref err) => Some(err),
            Error::Utf8(ref err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error {
        Error::Io(err)
    }
}

impl From<FromUtf8Error> for Error {
    fn from(err: FromUtf8Error) -> Error {
        Error::Utf8(err)
    }
}

#[test]
fn test_intern() {
    let mut state = State::new();
    let ls: LuaString = state.intern("test");
    state.push(ls).unwrap();
    let rs: String = state.at(LuaIndex::Stack(-1)).unwrap();
    state.pop(1);
    assert!(rs == "test");
    assert!(state.get_top() == 0);
}
