#![feature(try_from, core_intrinsics)]

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
pub type NativeFunction = fn(&mut State) -> RunResult<u32>;

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
    /// Convert the index to a stack index if possible, otherwise panic.
    pub fn to_stack(&self) -> i32 {
        match *self {
            LuaIndex::Stack(val) => val,
            _ => unreachable!(),
        }
    }

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

/// A result which may return a Lua load-time error.
pub type LoadResult<T> = result::Result<T, LoadError>;

/// Describes a Lua load-time error.
#[derive(Debug)]
pub enum LoadError {
    /// An IO error occurred.
    Io(io::Error),
    /// A UTF-8 conversion error occurred.
    Utf8(FromUtf8Error),
    /// A syntax error occurred.
    Syntax(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            LoadError::Io(ref err) => err.fmt(f),
            LoadError::Utf8(ref err) => err.fmt(f),
            LoadError::Syntax(ref msg) => write!(f, "{}", msg),
        }
    }
}

impl error::Error for LoadError {
    fn description(&self) -> &str {
        match *self {
            LoadError::Io(ref err) => err.description(),
            LoadError::Utf8(ref err) => err.description(),
            LoadError::Syntax(_) => "Lua syntax error",
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            LoadError::Io(ref err) => Some(err),
            LoadError::Utf8(ref err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for LoadError {
    fn from(err: io::Error) -> LoadError {
        LoadError::Io(err)
    }
}

impl From<FromUtf8Error> for LoadError {
    fn from(err: FromUtf8Error) -> LoadError {
        LoadError::Utf8(err)
    }
}

/// A result which may return a Lua runtime error.
pub type RunResult<T> = result::Result<T, RunError>;

/// Describes a Lua run-time error.
#[derive(Debug)]
pub struct RunError {
    pub message: String,
    pub backtrace: Vec<String>,
}

impl RunError {
    /// Generate an error with the given message and backtrace.
    pub fn new(message: String, backtrace: Vec<String>) -> RunError {
        RunError {
            message: message,
            backtrace: backtrace,
        }
    }

    /// Generate a type conversion error message (Lua -> Rust)
    pub fn conversion_from_lua(src_type: Option<LuaType>,
                               dst_type: &'static str,
                               backtrace: Vec<String>)
                               -> RunError {
        let message = match src_type {
            Some(ty) => {
                format!("invalid conversion from Lua type `{:?}` to Rust type `{}`",
                        ty,
                        dst_type)
            }
            None => format!("invalid index"),
        };
        RunError {
            message: message,
            backtrace: backtrace,
        }
    }

    /// Generate a type conversion error message (Rust -> Lua)
    pub fn conversion_to_lua(src_type: &'static str,
                             dst_type: LuaType,
                             backtrace: Vec<String>)
                             -> RunError {
        let message = format!("invalid conversion from Rust type `{}` to Lua type `{:?}`",
                              src_type,
                              dst_type);
        RunError {
            message: message,
            backtrace: backtrace,
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl error::Error for RunError {
    fn description(&self) -> &str {
        "Lua runtime error"
    }

    fn cause(&self) -> Option<&error::Error> {
        None
    }
}

#[test]
fn test_intern() {
    let mut state = State::new();
    let ls: LuaString = state.intern("test");
    state.push(ls);
    let rs: String = state.at(LuaIndex::Stack(-1)).unwrap();
    state.pop(1);
    assert!(rs == "test");
    assert!(state.get_top() == 0);
}

#[test]
fn test_userdata() {
    use std::rc::Rc;
    use std::cell::RefCell;

    struct HugeData {
        data: String,
    }

    impl HugeData {
        fn new() -> HugeData {
            HugeData { data: "This is some huge data".to_string() }
        }
    }

    impl Drop for HugeData {
        fn drop(&mut self) {
            println!("Dropping HugeData");
        }
    }

    fn test_function(state: &mut State) -> RunResult<u32> {
        let ref_ref: &mut Rc<RefCell<HugeData>> = try!(state.userdata_at(LuaIndex::Stack(1)));
        let obj_ref = ref_ref.borrow();
        println!("{}", obj_ref.data);
        Ok(0)
    }

    let bighuge_data = Rc::new(RefCell::new(HugeData::new()));
    let mut state = State::new();
    state.push_function(test_function);
    state.push_userdata(bighuge_data);
    state.call(1, LuaCallResults::Num(0)).unwrap();
}

#[test]
#[should_panic]
fn test_panic() {
    let mut state = State::new();
    fn test_function(_state: &mut State) -> RunResult<u32> {
        panic!("Test panic~");
    }
    state.push_function(test_function);
    state.call(0, LuaCallResults::Num(0)).unwrap();
}

#[test]
fn test_error() {
    let mut state = State::new();
    fn test_function(state: &mut State) -> RunResult<u32> {
        Err(RunError::new("Test error~".to_string(), state.backtrace()))
    }
    state.push_function(test_function);
    let result = state.call(0, LuaCallResults::Num(0));
    assert!(result.is_err() && result.err().unwrap().message == "Test error~");
}
