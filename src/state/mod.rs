mod traits;

use std::{io, ptr};
use std::ffi::{CStr, CString};
use std::mem;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::any::{Any, TypeId};
use std::intrinsics::type_name;
use std::panic::{self, AssertUnwindSafe};
use libc::{self, c_int, c_char, size_t, c_void};

use ffi;
use super::{LoadResult, LoadError, RunResult, RunError, LuaType, LuaOperator, LuaCallResults,
            LuaIndex, LuaString, NativeFunction};
pub use self::traits::*;

/// The userdata memory stored in Lua.
struct Userdata<T: Any> {
    type_id: TypeId,
    value: T,
}

/// Contains the Lua state.
///
/// See the [module level documentation](index.html) for more details.
pub struct State {
    lua: *mut ffi::lua_State,
    should_free: bool,
}

impl State {
    /// Creates a new Lua state. This function can panic if state creation fails, though this
    /// only happens in extreme scenarios such as insufficient memory.
    pub fn new() -> State {
        // Create the Lua state through the FFI
        let lua = unsafe { ffi::lua_newstate(alloc, ptr::null_mut()) };
        extern "C" fn alloc(_ud: *mut c_void,
                            ptr: *mut c_void,
                            _osize: size_t,
                            nsize: size_t)
                            -> *mut c_void {
            unsafe {
                if nsize == 0 {
                    libc::free(ptr as *mut c_void);
                    ptr::null_mut()
                } else {
                    libc::realloc(ptr, nsize)
                }
            }
        }

        if lua.is_null() {
            panic!("lua_newstate failed");
        }

        // UNDONE: Set the panic handler
        // Since panicking and unwinding the stack is undefined behavior, just let Lua abort for us
        // instead of causing a mess.
        // unsafe { ffi::lua_atpanic(lua, panic) };
        // extern "C" fn panic(lua: *mut ffi::lua_State) -> c_int {
        // let mut state = State::from_raw_state(lua);
        // let err = state.to_string(LuaIndex::Stack(-1)).unwrap();
        // panic!("PANIC: unprotected error in call to Lua API ({})", err);
        // }


        // Create the state object
        let mut state = State {
            lua: lua,
            should_free: true,
        };

        // Add the address of the State object to the "extradata" and create a table in the
        // registry. The address isn't actually used to track the location of the object, but the
        // usage of an address for the key of a registry table was recommended by the Lua 5.3
        // reference manual.
        //
        // From here, populate our registry table with some important values:
        // * `errfunc`: A function called to generate a backtrace on a Lua runtime error.
        // * `string`: A table that maps internal string pointers to their corresponding string
        //             values.
        // * `mt`: A table that maps `TypeId` hashes to their corresponding userdata metatables.
        // * `user`: A table reserved for external crate use returned by `get_registry()`.
        unsafe {
            let extraspace = ffi::lua_getextraspace(state.lua) as *mut *mut c_void;
            *extraspace = (&mut state as *mut State) as *mut c_void;
            ffi::lua_newtable(state.lua);
            // errfunc
            ffi::lua_pushcfunction(state.lua, errfunc);
            ffi::lua_setfield(state.lua, -2, b"errfunc\0".as_ptr() as *const c_char);
            // string
            ffi::lua_newtable(state.lua);
            ffi::lua_setfield(state.lua, -2, b"string\0".as_ptr() as *const c_char);
            // mt
            ffi::lua_newtable(state.lua);
            ffi::lua_setfield(state.lua, -2, b"mt\0".as_ptr() as *const c_char);
            // user
            ffi::lua_newtable(state.lua);
            ffi::lua_setfield(state.lua, -2, b"user\0".as_ptr() as *const c_char);
            // save to registry
            ffi::lua_rawsetp(state.lua, ffi::LUA_REGISTRYINDEX, *extraspace);
        }

        extern "C" fn errfunc(lua: *mut ffi::lua_State) -> c_int {
            // Coerce the error value into a RunError with a backtrace, unless it's a PanicError.
            let mut state = State::from_raw_state(lua);
            if state.is_userdata_of_type::<RunError>(LuaIndex::Stack(1)) ||
               state.is_userdata_of_type::<Box<Any + Send>>(LuaIndex::Stack(1)) {
                // do nothing
            } else {
                // Coerce to RunError
                let message = match state.at::<String>(LuaIndex::Stack(1)) {
                    Ok(val) => val,
                    Err(_) => "unknown error".to_string(),
                };
                let bt = state.backtrace();
                state.push_userdata(RunError::new(message, bt));
            }
            1
        }
        state
    }

    /// Opens all standard Lua libraries into the state.
    pub fn open_libs(&mut self) {
        unsafe { ffi::luaL_openlibs(self.lua) }
    }

    /// Load string containing Lua code as a Lua function on the top of the stack.
    /// If an error occurs, nothing is pushed to the stack.
    pub fn load_string(&mut self, str: &str, chunkname: &str) -> LoadResult<()> {
        // TODO: special case for strings so there's not so much memory movement?
        let vec = str.as_bytes().to_vec();
        self.load_stream(vec.as_slice(), chunkname)
    }

    /// Load string containing Lua code as a Lua function on the top of the stack.
    /// If an error occurs, nothing is pushed to the stack.
    pub fn load_stream<R: io::Read>(&mut self, stream: R, chunkname: &str) -> LoadResult<()> {
        extern "C" fn reader<R: io::Read>(_lua: *mut ffi::lua_State,
                                          data: *mut c_void,
                                          size: *mut size_t)
                                          -> *const c_char {
            unsafe {
                let rd = &mut *(data as *mut ReaderData<R>);
                rd.string.truncate(0);
                rd.stream.read_to_end(&mut rd.string).unwrap();
                *size = rd.string.len() as size_t;
                rd.string.as_ptr() as *const c_char
            }
        }

        struct ReaderData<R: io::Read> {
            stream: R,
            string: Vec<u8>,
        }

        let mut data = ReaderData {
            stream: stream,
            string: Vec::new(),
        };

        let result = unsafe {
            ffi::lua_load(self.lua,
                          reader::<R>,
                          (&mut data as *mut ReaderData<R>) as *mut c_void,
                          CString::new(chunkname).unwrap().as_ptr(),
                          ptr::null())
        };
        self.lua_to_rust_load_result(result)
    }

    /// Calls a function.
    ///
    /// To call a function you must use the following protocol: first, the function to be called is
    /// pushed onto the stack; then, the arguments to the function are pushed in direct order;
    /// that is, the first argument is pushed first. Finally you call `call()`; `nargs` is the
    /// number of arguments that you pushed onto the stack. All arguments and the function value are
    /// popped from the stack when the function is called. The function results are pushed onto the
    /// stack when the function returns. The number of results is adjusted to `results`, unless
    /// `results` is `LuaCallResults::MultRet`. In this case, all results from the function are
    /// pushed. Lua takes care that the returned values fit into the stack space, but it does not
    /// ensure any extra space in the stack. The function results are pushed onto the stack in
    /// direct order (the first result is pushed first), so that after the call the last result is
    /// on the top of the stack.
    pub fn call(&mut self, nargs: u32, results: LuaCallResults) -> RunResult<()> {
        let nresults = match results {
            LuaCallResults::Num(val) => val as c_int,
            LuaCallResults::MultRet => ffi::LUA_MULTRET,
        };
        self.get_internal_registry();
        self.get_field(LuaIndex::Stack(-1), "errfunc");
        self.remove(-2);
        let errfunc_idx = self.abs_index(LuaIndex::Stack(-(nargs as i32) - 2)).to_stack();
        self.insert(errfunc_idx);
        let result =
            unsafe { ffi::lua_pcall(self.lua, nargs as c_int, nresults, errfunc_idx as c_int) };
        self.remove(errfunc_idx);
        self.lua_to_rust_run_result(result)
    }

    /// Push a type on the top of the stack.
    pub fn push<T: ToLua>(&mut self, val: T) {
        val.to_lua(self);
    }

    /// Pushes a native function onto the stack. This function receives a pointer to a native
    /// function and pushes onto the stack a Lua value of type function that, when called, invokes
    /// the corresponding native function.
    ///
    /// Any function to be callable by Lua must follow the correct protocol to receive its
    /// parameters and return its results ([see `NativeFunction`](NativeFunction.t.html)).
    pub fn push_function(&mut self, f: NativeFunction) {
        self.push_closure(f, 0);
    }

    /// Pushes a new closure onto the stack. Note that this is *not* a Rust closure, due to
    /// lifetime tracking limitations.
    ///
    /// When a native function is created, it is possible to associate some values with it, thus
    /// creating a native closure ([see here](https://www.lua.org/manual/5.3/manual.html#4.4));
    /// these values are then accessible to the function whenever it is called. To associate values
    /// with a native function, first these values must be pushed onto the stack (when there are
    /// multiple values, the first value is pushed first). Then `push_closure()` is called to create
    /// and push the native function onto the stack, with the argument `n` telling how many values
    /// will be associated with the function. `push_closure()` also pops these values from the
    /// stack.
    ///
    /// The maximum value for n is 254. This differs from standard C Lua, as lowlua needs the first
    /// index internally.
    pub fn push_closure(&mut self, f: NativeFunction, n: u32) {
        extern "C" fn func(lua: *mut ffi::lua_State) -> c_int {
            unsafe {
                let f =
                    &*(ffi::lua_touserdata(lua, ffi::lua_upvalueindex(1)) as *mut NativeFunction);
                let mut state = State::from_raw_state(lua);
                // Call function and catch panics
                let panic_result = panic::catch_unwind(AssertUnwindSafe(|| f(&mut state)));
                match panic_result {
                    // No panic
                    Ok(result) => {
                        match result {
                            Ok(val) => val as c_int,
                            Err(err) => {
                                state.push_userdata(err);
                                ffi::lua_error(state.lua);
                                0 // unreachable
                            }
                        }
                    }
                    // Panic!
                    Err(err) => {
                        state.push_userdata(err);
                        ffi::lua_error(state.lua);
                        0 // unreachable
                    }
                }
            }
        }

        unsafe {
            // Push userdata instead of light userdata, as some platforms may have differing
            // pointer sizes between functions and variables.
            let ud = ffi::lua_newuserdata(self.lua, mem::size_of::<NativeFunction>()) as *mut NativeFunction;
            *ud = f;
            let n = (n + 1) as i32;
            if n > 1 {
                self.insert(-n);
            }
            ffi::lua_pushcclosure(self.lua, func, n);
        }
    }

    /// Transfers the object referred to by `value` into a Lua userdata object and sets the
    /// appropriate metatable.
    ///
    /// This type can be later accessed by `userdata_at()` with safe type-checking, as lowlua
    /// uses `std::any` internally to keep track of userdata types.
    pub fn push_userdata<T: Any>(&mut self, value: T) {
        unsafe {
            // Push to stack
            let ud = Userdata {
                type_id: TypeId::of::<T>(),
                value: value,
            };
            let ptr =
                ffi::lua_newuserdata(self.lua, mem::size_of::<Userdata<T>>()) as *mut Userdata<T>;
            ptr::write(ptr, ud);

            // Associate metatable
            self.get_metatable_of::<T>();
            ffi::lua_setmetatable(self.lua, -2);
        }
    }

    /// Pushes a `nil` value onto the stack.
    pub fn push_nil(&mut self) {
        unsafe { ffi::lua_pushnil(self.lua) }
    }

    /// Get a type from a place on the stack.
    pub fn at<T: FromLua>(&mut self, idx: LuaIndex) -> RunResult<T> {
        let top = self.get_top();
        let result = T::from_lua(self, idx);
        self.set_top(top);
        result
    }

    /// Returns a reference to the data stored in the userdata value at the given index, given that
    /// the value is userdata (not including light userdata) and the type matches `T`.
    /// Otherwise, returns an `Error::Type`.
    pub fn userdata_at<'a, T: Any>(&self, idx: LuaIndex) -> RunResult<&'a mut T> {
        unsafe {
            if ffi::lua_type(self.lua, idx.to_ffi()) == ffi::LUA_TUSERDATA {
                let ptr = ffi::lua_touserdata(self.lua, idx.to_ffi()) as *mut Userdata<T>;
                let ud = &mut *ptr;
                if ud.type_id == TypeId::of::<T>() {
                    Ok(&mut ud.value)
                } else {
                    Err(RunError::conversion_from_lua(Some(LuaType::Userdata),
                                                      type_name::<T>(),
                                                      self.backtrace()))
                }
            } else {
                Err(RunError::conversion_from_lua(self.type_at(idx),
                                                  type_name::<T>(),
                                                  self.backtrace()))
            }
        }
    }

    /// Moves the userdata out of Lua back into Rust. The userdata will still exist in Lua, as
    /// it's not possible (or a good idea) to remove every reference to it, but will be rendered
    /// completely inert as it's metatable will be stripped.
    pub fn userdata_move<T: Any>(&self, idx: LuaIndex) -> RunResult<T> {
        unsafe {
            let idx = self.abs_index(idx);
            // Suck the error right back out of Lua
            // First clear the metatable so the error isn't double-freed by Lua
            ffi::lua_pushnil(self.lua);
            ffi::lua_setmetatable(self.lua, idx.to_ffi());
            // Now copy back into Rust managed memory
            let lua_obj = try!(self.userdata_at::<T>(idx));
            let mut obj = mem::uninitialized::<T>();
            ptr::copy_nonoverlapping(lua_obj as *const T, &mut obj as *mut T, 1);
            Ok(obj)
        }
    }

    /// Converts the acceptable index idx into an equivalent absolute index (that is, one that does
    /// not depend on the stack top).
    pub fn abs_index(&self, idx: LuaIndex) -> LuaIndex {
        match idx {
            LuaIndex::Stack(_) => {
                LuaIndex::Stack(unsafe { ffi::lua_absindex(self.lua, idx.to_ffi()) as i32 })
            }
            _ => idx,
        }
    }

    /// Returns the index of the top element in the stack. Because indices start at 1, this result
    /// is equal to the number of elements in the stack; in particular, 0 means an empty stack.
    pub fn get_top(&self) -> i32 {
        unsafe { ffi::lua_gettop(self.lua) as i32 }
    }

    /// Accepts any index, or 0, and sets the stack top to this index. If the new top is larger than
    /// the old one, then the new elements are filled with nil. If index is 0, then all stack
    /// elements are removed.
    pub fn set_top(&mut self, idx: i32) {
        assert!(idx >= 0);
        unsafe { ffi::lua_settop(self.lua, idx as c_int) }
    }

    /// Pushes a copy of the element at the given index onto the stack.
    pub fn push_value(&mut self, idx: LuaIndex) {
        unsafe { ffi::lua_pushvalue(self.lua, idx.to_ffi()) }
    }

    /// Rotates the stack elements between the valid index idx and the top of the stack. The
    /// elements are rotated `n` positions in the direction of the top, for a positive `n`, or
    /// `-n` positions in the direction of the bottom, for a negative `n`. The absolute value of `n`
    /// must not be greater than the size of the slice being rotated. This function cannot be called
    /// with a pseudo-index, because a pseudo-index is not an actual stack position.
    pub fn rotate(&mut self, idx: i32, n: i32) {
        unsafe { ffi::lua_rotate(self.lua, idx as c_int, n as c_int) }
    }

    /// Copies the element at index `fromidx` into the valid index `toidx`, replacing the value at
    /// that position. Values at other positions are not affected.
    pub fn copy(&mut self, fromidx: i32, toidx: i32) {
        unsafe { ffi::lua_copy(self.lua, fromidx as c_int, toidx as c_int) }
    }

    /// Pop `n` elements from the stack.
    pub fn pop(&mut self, n: i32) {
        unsafe { ffi::lua_pop(self.lua, n as c_int) }
    }

    /// Moves the top element into the given valid index, shifting up the elements above this index
    /// to open space. This function cannot be called with a pseudo-index, because a pseudo-index
    /// is not an actual stack position.
    pub fn insert(&mut self, idx: i32) {
        unsafe { ffi::lua_insert(self.lua, idx as c_int) }
    }

    /// Removes the element at the given valid index, shifting down the elements above this index to
    /// fill the gap. This function cannot be called with a pseudo-index, because a pseudo-index is
    /// not an actual stack position.
    pub fn remove(&mut self, idx: i32) {
        unsafe { ffi::lua_remove(self.lua, idx as c_int) }
    }

    /// Moves the top element into the given valid index without shifting any element (therefore
    /// replacing the value at that given index), and then pops the top element.
    pub fn replace(&mut self, idx: i32) {
        unsafe { ffi::lua_replace(self.lua, idx as c_int) }
    }

    /// Ensures that the stack has space for at least `n` extra slots (that is, that you can safely
    /// push up to `n` values into it). It returns `false` if it cannot fulfill the request, either
    /// because it would cause the stack to be larger than a fixed maximum size (typically at least
    /// several thousand elements) or because it cannot allocate memory for the extra space. This
    /// function never shrinks the stack; if the stack already has space for the extra slots, it
    /// is left unchanged.
    pub fn check_stack(&self, n: i32) -> bool {
        unsafe { ffi::lua_checkstack(self.lua, n as c_int) != 0 }
    }

    /// Returns `true` if the value at the given index is a number or a string convertible to a
    /// number.
    pub fn is_number(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isnumber(self.lua, idx.to_ffi()) != 0 }
    }

    /// Returns `true` if the value at the given index is a string or a number (which is always
    /// convertible to a string).
    pub fn is_string(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isstring(self.lua, idx.to_ffi()) != 0 }
    }

    /// Returns `true` if the value at the given index is a native function.
    pub fn is_native_function(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_iscfunction(self.lua, idx.to_ffi()) != 0 }
    }

    /// Returns `true` if the value at the given index is an integer (that is, the value is a number
    /// and is represented as an integer).
    pub fn is_integer(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isinteger(self.lua, idx.to_ffi()) != 0 }
    }

    /// Returns `true` if the value at the given index is a userdata (either full or light).
    pub fn is_userdata(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isuserdata(self.lua, idx.to_ffi()) != 0 }
    }

    /// Returns `true` if the given index is a userdata of the given type.
    pub fn is_userdata_of_type<T: Any>(&self, idx: LuaIndex) -> bool {
        unsafe {
            if ffi::lua_type(self.lua, idx.to_ffi()) == ffi::LUA_TUSERDATA {
                let ptr = ffi::lua_touserdata(self.lua, idx.to_ffi()) as *mut Userdata<T>;
                let ud = &mut *ptr;
                ud.type_id == TypeId::of::<T>()
            } else {
                false
            }
        }
    }

    /// Returns `true` if the value at the given index is a function (either native or Lua).
    pub fn is_function(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isfunction(self.lua, idx.to_ffi()) }
    }

    /// Returns `true` if the value at the given index is a table.
    pub fn is_table(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_istable(self.lua, idx.to_ffi()) }
    }

    /// Returns `true` if the value at the given index is a light userdata.
    pub fn is_light_userdata(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_islightuserdata(self.lua, idx.to_ffi()) }
    }

    /// Returns `true` if the value at the given index is `nil`.
    pub fn is_nil(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isnil(self.lua, idx.to_ffi()) }
    }

    /// Returns `true` if the value at the given index is a boolean.
    pub fn is_boolean(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isboolean(self.lua, idx.to_ffi()) }
    }

    /// Returns `true` if the value at the given index is a thread.
    pub fn is_thread(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isthread(self.lua, idx.to_ffi()) }
    }

    /// Returns `true` if the given index is not valid.
    pub fn is_none(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isnone(self.lua, idx.to_ffi()) }
    }

    /// Returns `true` if the given index is not valid or if the value at this index is `nil`.
    pub fn is_none_or_nil(&self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_isnoneornil(self.lua, idx.to_ffi()) }
    }

    /// Returns the `LuaType` of the value in the given valid index, or `None` for a non-valid
    /// (but acceptable) index.
    pub fn type_at(&self, idx: LuaIndex) -> Option<LuaType> {
        lua_to_rust_type_checked(unsafe { ffi::lua_type(self.lua, idx.to_ffi()) })
    }

    /// Returns the raw "length" of the value at the given index: for strings, this is the string
    /// length; for tables, this is the result of the length operator ('#') with no metamethods;
    /// for userdata, this is the size of the block of memory allocated for the userdata; for other
    /// values, it is 0.
    pub fn raw_len(&self, idx: LuaIndex) -> usize {
        unsafe { ffi::lua_rawlen(self.lua, idx.to_ffi()) as usize }
    }

    /// Performs an arithmetic or bitwise operation over the two values (or one, in the case of
    /// negations) at the top of the stack, with the value at the top being the second operand,
    /// pops these values, and pushes the result of the operation. The function follows the
    /// semantics of the corresponding Lua operator (that is, it may call metamethods).
    pub fn arith(&self, op: LuaOperator) {
        unsafe { ffi::lua_arith(self.lua, rust_to_lua_op(op)) }
    }

    /// Returns `true` if the two values in indices `idx1` and `idx2` are primitively equal
    /// (that is, without calling the `__eq` metamethod). Otherwise returns `false`.
    /// Also returns `false` if any of the indices are not valid.
    pub fn raw_equal(&self, idx1: LuaIndex, idx2: LuaIndex) -> bool {
        unsafe { ffi::lua_rawequal(self.lua, idx1.to_ffi(), idx2.to_ffi()) != 0 }
    }

    /// Compares two Lua values. Returns `true` if the value at index `idx1` satisfies `op`
    /// when compared with the value at index `idx2`, following the semantics of the corresponding
    /// Lua operator (that is, it may call metamethods). Otherwise returns `false`.
    /// Also returns `false` if any of the indices is not valid.
    pub fn compare(&mut self, idx1: LuaIndex, idx2: LuaIndex, op: LuaOperator) -> bool {
        unsafe { ffi::lua_compare(self.lua, idx1.to_ffi(), idx2.to_ffi(), rust_to_lua_op(op)) != 0 }
    }

    /// Pushes onto the stack the value of the global `name`. Returns the `LuaType` of that value.
    pub fn get_global(&mut self, name: &str) -> LuaType {
        lua_to_rust_type(unsafe {
            ffi::lua_getglobal(self.lua, CString::new(name).unwrap().as_ptr())
        })
    }

    /// Pushes onto the stack the value `t[k]`, where `t` is the value at the given index and
    /// `k` is the value at the top of the stack.
    ///
    /// This function pops the key from the stack, pushing the resulting value in its place.
    /// As in Lua, this function may trigger a metamethod for the "index" event
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.4)).
    ///
    /// Returns the `LuaType` of the pushed value.
    pub fn get_table(&mut self, idx: LuaIndex) -> LuaType {
        lua_to_rust_type(unsafe { ffi::lua_gettable(self.lua, idx.to_ffi()) })
    }

    /// Pushes onto the stack the value `t[k]`, where `t` is the value at the given index.
    /// As in Lua, this function may trigger a metamethod for the "index" event
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.4)).
    ///
    /// Returns the `LuaType` of the pushed value.
    pub fn get_field(&mut self, idx: LuaIndex, k: &str) -> LuaType {
        lua_to_rust_type(unsafe {
            ffi::lua_getfield(self.lua, idx.to_ffi(), CString::new(k).unwrap().as_ptr()) as i32
        })
    }

    /// Pushes onto the stack the value `t[n]`, where `t` is the value at the given index.
    /// As in Lua, this function may trigger a metamethod for the "index" event
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.4)).
    ///
    /// Returns the `LuaType` of the pushed value.
    pub fn get_i(&mut self, idx: LuaIndex, n: i64) -> LuaType {
        lua_to_rust_type(unsafe { ffi::lua_geti(self.lua, idx.to_ffi(), n as ffi::lua_Integer) })
    }

    /// Similar to `get_table()`, but does a raw access (i.e., without metamethods).
    ///
    /// Returns the `LuaType` of the pushed value.
    pub fn raw_get(&mut self, idx: LuaIndex) -> LuaType {
        lua_to_rust_type(unsafe { ffi::lua_rawget(self.lua, idx.to_ffi()) })
    }

    /// Pushes onto the stack the value `t[n]`, where `t` is the table at the given index.
    /// The access is raw, that is, it does not invoke the `__index` metamethod.
    ///
    /// Returns the `LuaType` of the pushed value.
    pub fn raw_get_i(&mut self, idx: LuaIndex, n: i64) -> LuaType {
        lua_to_rust_type(unsafe { ffi::lua_rawgeti(self.lua, idx.to_ffi(), n as ffi::lua_Integer) })
    }

    /// Pushes onto the stack the value `t[k]`, where `t` is the table at the given index and
    /// `k` is the pointer `p` represented as a light userdata. The access is raw; that is,
    /// it does not invoke the `__index` metamethod.
    ///
    /// Returns the type of the pushed value.
    pub fn raw_get_p<T>(&mut self, idx: LuaIndex, p: *const T) -> LuaType {
        unsafe { lua_to_rust_type(ffi::lua_rawgetp(self.lua, idx.to_ffi(), p as *const c_void)) }
    }

    /// Creates a new empty table and pushes it onto the stack.
    /// It is equivalent to `create_table(0, 0)`
    pub fn new_table(&mut self) {
        unsafe { ffi::lua_newtable(self.lua) };
    }

    /// Creates a new empty table and pushes it onto the stack. Parameter `narr` is a hint for
    /// how many elements the table will have as a sequence; parameter `nrec` is a hint for how
    /// many other elements the table will have. Lua may use these hints to preallocate memory
    /// for the new table. This preallocation is useful for performance when you know in advance
    /// how many elements the table will have. Otherwise you can use the function `new_table()`.
    pub fn create_table(&mut self, narr: i32, nrec: i32) {
        unsafe { ffi::lua_createtable(self.lua, narr as c_int, nrec as c_int) }
    }

    /// If the value at the given index has a metatable, the function pushes that metatable onto
    /// the stack and returns `true`. Otherwise, the function returns `false` and pushes nothing
    /// on the stack.
    pub fn get_metatable(&mut self, objindex: LuaIndex) -> bool {
        unsafe { ffi::lua_getmetatable(self.lua, objindex.to_ffi()) != 0 }
    }

    /// Gets (or creates) the metatable associated with the specified userdata type and pushes it
    /// onto the top of the stack. This can be used to extend the functionality of a userdata type.
    pub fn get_metatable_of<T: Any>(&mut self) {
        extern "C" fn gc<T: Any>(lua: *mut ffi::lua_State) -> c_int {
            unsafe {
                let ptr = ffi::lua_touserdata(lua, 1) as *mut Userdata<T>;
                ptr::drop_in_place(ptr);
                0
            }
        }
        // First, get a hash of the type, which is used to look up the appropriate metatable
        let mut hasher = DefaultHasher::new();
        TypeId::of::<T>().hash(&mut hasher);
        let mt_key = hasher.finish() as ffi::lua_Integer;
        // Now look up/create the table
        unsafe {
            self.get_internal_registry();
            ffi::lua_getfield(self.lua, -1, b"mt\0".as_ptr() as *const c_char);
            ffi::lua_remove(self.lua, -2);
            ffi::lua_rawgeti(self.lua, -1, mt_key);
            if ffi::lua_isnil(self.lua, -1) {
                ffi::lua_pop(self.lua, 1);
                // Create the metatable
                ffi::lua_newtable(self.lua);
                ffi::lua_pushcfunction(self.lua, gc::<T>);
                ffi::lua_setfield(self.lua, -2, b"__gc\0".as_ptr() as *const c_char);
                ffi::lua_pushvalue(self.lua, -1);
                ffi::lua_rawseti(self.lua, -3, mt_key);
            }
            ffi::lua_remove(self.lua, -2);
        }
    }

    /// Pushes onto the stack the Lua value associated with the userdata at the given index.
    ///
    /// Returns the type of the pushed value.
    pub fn get_uservalue(&mut self, idx: LuaIndex) -> LuaType {
        lua_to_rust_type(unsafe { ffi::lua_getuservalue(self.lua, idx.to_ffi()) })
    }

    /// Pushes our registry table onto the stack. When the Lua state is crated, a table is
    /// automatically allocated in the registry that can be freely used by the program.
    pub fn get_registry(&mut self) {
        self.get_internal_registry();
        self.get_field(LuaIndex::Stack(-1), "user");
        self.remove(-2);
    }

    /// Pops a value from the stack and sets it as the new value of global `name`.
    pub fn set_global(&mut self, name: &str) {
        unsafe { ffi::lua_setglobal(self.lua, CString::new(name).unwrap().as_ptr()) }
    }

    /// Does the equivalent to `t[k] = v`, where `t` is the value at the given index, `v` is the
    /// value at the top of the stack, and `k` is the value just below the top.
    ///
    /// This function pops both the key and the value from the stack. As in Lua, this function may
    /// trigger a metamethod for the "newindex" event
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.4)).
    pub fn set_table(&mut self, idx: LuaIndex) {
        unsafe { ffi::lua_settable(self.lua, idx.to_ffi()) }
    }

    /// Does the equivalent to `t[k] = v`, where `t` is the value at the given index and `v` is the
    /// value at the top of the stack.
    ///
    /// This function pops the value from the stack. As in Lua, this function may trigger a
    /// metamethod for the "newindex" event
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.4)).
    pub fn set_field(&mut self, idx: LuaIndex, k: &str) {
        unsafe { ffi::lua_setfield(self.lua, idx.to_ffi(), CString::new(k).unwrap().as_ptr()) }
    }

    /// Does the equivalent to `t[n] = v`, where `t` is the value at the given index and
    /// `v` is the value at the top of the stack.
    ///
    /// This function pops the value from the stack. As in Lua, this function may trigger a
    /// metamethod for the "newindex" event
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.4)).
    pub fn set_i(&mut self, idx: LuaIndex, n: i64) {
        unsafe { ffi::lua_seti(self.lua, idx.to_ffi(), n as ffi::lua_Integer) }
    }

    /// Similar to `set_table()`, but does a raw assignment (i.e., without metamethods).
    pub fn raw_set(&mut self, idx: LuaIndex) {
        unsafe { ffi::lua_rawset(self.lua, idx.to_ffi()) }
    }

    /// Does the equivalent of `t[n] = v`, where `t` is the table at the given index and
    /// `v` is the value at the top of the stack.
    ///
    /// This function pops the value from the stack. The assignment is raw, that is,
    /// it does not invoke the `__newindex` metamethod.
    pub fn raw_set_i(&mut self, idx: LuaIndex, n: i64) {
        unsafe { ffi::lua_rawseti(self.lua, idx.to_ffi(), n as ffi::lua_Integer) }
    }

    /// Does the equivalent of `t[p] = v`, where `t` is the table at the given index,
    /// `p` is encoded as a light userdata, and `v` is the value at the top of the stack.
    ///
    /// This function pops the value from the stack. The assignment is raw, that is,
    /// it does not invoke the `__newindex` metamethod.
    pub fn raw_set_p<T>(&mut self, idx: LuaIndex, p: *const T) {
        unsafe {
            ffi::lua_rawsetp(self.lua, idx.to_ffi(), p as *const c_void);
        }
    }

    /// Pops a table from the stack and sets it as the new metatable for the value at the
    /// given index.
    pub fn set_metatable(&mut self, objindex: LuaIndex) {
        unsafe { ffi::lua_setmetatable(self.lua, objindex.to_ffi()) };
    }

    /// Pops a value from the stack and sets it as the new value associated to the userdata at the
    /// given index.
    pub fn set_uservalue(&mut self, idx: LuaIndex) {
        unsafe { ffi::lua_setuservalue(self.lua, idx.to_ffi()) }
    }

    /// Sets the native function `f` as the new value of global `name`.
    pub fn register(&mut self, name: &str, f: NativeFunction) {
        self.push_function(f);
        self.set_global(name);
    }

    /// Stops the garbage collector.
    pub fn gc_stop(&mut self) {
        unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCSTOP, 0) };
    }

    /// Restarts the garbage collector.
    pub fn gc_restart(&mut self) {
        unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCRESTART, 0) };
    }

    /// Performs a full garbage-collection cycle.
    pub fn gc_collect(&mut self) {
        unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCCOLLECT, 0) };
    }

    /// Returns the current amount of memory (in bytes) in use by Lua.
    pub fn gc_count(&self) -> usize {
        let size = unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCCOUNT, 0) as usize } * 1024;
        size + unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCCOUNTB, 0) as usize }
    }

    /// Performs an incremental step of garbage collection.
    pub fn gc_step(&mut self) {
        unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCSTEP, 0) };
    }

    /// Sets `pause` as the new value for the "pause" of the collector
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.5)) and returns the previous value
    /// of the pause.
    pub fn gc_set_pause(&mut self, pause: i32) -> i32 {
        unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCSETPAUSE, pause) }
    }

    /// Sets `stepmul` as the new value for the "step multiplier" of the collector
    /// (see [here](https://www.lua.org/manual/5.3/manual.html#2.5)) and returns the previous value
    /// of the step multiplier.
    pub fn gc_set_step_mul(&mut self, stepmul: i32) -> i32 {
        unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCSETSTEPMUL, stepmul) }
    }

    /// Returns a `bool` that tells whether the collector is running (i.e., not stopped).
    pub fn gc_is_running(&self) -> bool {
        unsafe { ffi::lua_gc(self.lua, ffi::LUA_GCISRUNNING, 0) != 0 }
    }

    /// Pops a key from the stack, and pushes a key–value pair from the table at the given index
    /// (the "next" pair after the given key). If there are no more elements in the table, then
    /// lua_next returns `false` (and pushes nothing).
    ///
    /// While traversing a table, do not call `as::<String>()` directly on a key, unless you know
    /// that the key is actually a string. Recall that `as::<String>()` may change the value at the
    /// given index; this confuses the next call to `next()`.
    pub fn next(&mut self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_next(self.lua, idx.to_ffi()) != 0 }
    }

    /// Concatenates the `n` values at the top of the stack, pops them, and leaves the result at
    /// the top. If `n` is 1, the result is the single value on the stack (that is, the function
    /// does nothing); if `n` is 0, the result is the empty string. Concatenation is performed
    /// following the usual semantics of Lua
    /// ([see here](https://www.lua.org/manual/5.3/manual.html#3.4.6)).
    pub fn concat(&mut self, n: i32) {
        unsafe { ffi::lua_concat(self.lua, n as c_int) }
    }

    /// Returns the length of the value at the given index. It is equivalent to the '#' operator
    /// in Lua ([see here](https://www.lua.org/manual/5.3/manual.html#3.4.7))
    /// and may trigger a metamethod for the "length" event
    /// ([see here](https://www.lua.org/manual/5.3/manual.html#2.4)).
    /// The result is pushed on the stack.
    pub fn len(&mut self, idx: LuaIndex) {
        unsafe { ffi::lua_len(self.lua, idx.to_ffi()) }
    }

    /// Converts the string `s` to a number, pushes that number into the stack, and returns `true`.
    /// The conversion can result in an integer or a float, according to the
    /// lexical conventions of Lua ([see here](https://www.lua.org/manual/5.3/manual.html#3.1)).
    /// The string may have leading and trailing spaces and a sign.
    ///
    /// If the string is not a valid numeral, returns `false` and pushes nothing.
    pub fn string_to_number(&mut self, s: &str) -> bool {
        unsafe { ffi::lua_stringtonumber(self.lua, CString::new(s).unwrap().as_ptr()) != 0 }
    }

    /// Creates a LuaString from the passed value. This value is interned and stored in the registry
    /// indefinitely.
    pub fn intern(&mut self, s: &str) -> LuaString {
        self.get_internal_registry();
        self.get_field(LuaIndex::Stack(-1), "string");
        self.push_string(s);
        let val = self.to_string_ptr(LuaIndex::Stack(-1)).unwrap() as usize;
        self.push_unsigned(val as u64);
        self.insert(-2);
        self.raw_set(LuaIndex::Stack(-3));
        self.pop(2);
        LuaString(val)
    }

    /// Generates a backtrace.
    pub fn backtrace(&self) -> Vec<String> {
        unsafe {
            let mut result = Vec::new();
            let mut debug = ffi::lua_Debug::default();
            let mut level = 0;
            while ffi::lua_getstack(self.lua, level, &mut debug as *mut ffi::lua_Debug) != 0 {
                ffi::lua_getinfo(self.lua,
                                 b"Sln\0".as_ptr() as *const c_char,
                                 &mut debug as *mut ffi::lua_Debug);
                let short_src = CStr::from_ptr(&debug.short_src as *const c_char).to_str().unwrap();
                let currentline = if debug.currentline > 0 {
                    format!("{}", debug.currentline)
                } else {
                    "".to_string()
                };
                let name = if *debug.namewhat != 0 {
                    let namewhat = CStr::from_ptr(debug.namewhat).to_str().unwrap();
                    let name = CStr::from_ptr(debug.name).to_str().unwrap();
                    format!("{} '{}'", namewhat, name)
                } else if *debug.what as u8 as char == 'm' {
                    "[main]".to_string()
                } else if *debug.what as u8 as char == 'C' {
                    "[native]".to_string()
                } else {
                    format!("function <{}:{}>", short_src, debug.linedefined)
                };
                result.push(format!("{}:{} in {}", short_src, currentline, name));
                level += 1;
            }
            result
        }
    }



    // Internal

    // This function is needed because it's unfortunately not possible to guarantee a reference
    // to the `State` that called a native function without making State boxed or something.
    fn from_raw_state(state: *mut ffi::lua_State) -> State {
        State {
            lua: state,
            should_free: false,
        }
    }

    // Misc
    /// Push the internal registry onto the stack. This table is not exposed to external crates.
    fn get_internal_registry(&mut self) {
        unsafe {
            let extraspace = ffi::lua_getextraspace(self.lua) as *const *mut c_void;
            ffi::lua_rawgetp(self.lua, ffi::LUA_REGISTRYINDEX, *extraspace);
        }
    }

    // Push

    fn push_number(&mut self, n: f64) {
        unsafe { ffi::lua_pushnumber(self.lua, n as ffi::lua_Number) }
    }

    fn push_integer(&mut self, n: i64) {
        unsafe { ffi::lua_pushinteger(self.lua, n as ffi::lua_Integer) }
    }

    fn push_string(&mut self, s: &str) {
        unsafe { ffi::lua_pushlstring(self.lua, s.as_ptr() as *const c_char, s.len() as size_t) }
    }

    fn push_boolean(&mut self, b: bool) {
        unsafe { ffi::lua_pushboolean(self.lua, if b { 1 } else { 0 }) }
    }

    fn push_unsigned(&mut self, n: u64) {
        unsafe { ffi::lua_pushunsigned(self.lua, n as ffi::lua_Unsigned) }
    }

    // To

    fn to_number(&mut self, idx: LuaIndex) -> RunResult<f64> {
        unsafe {
            let mut isnum: c_int = 0;
            let num = ffi::lua_tonumberx(self.lua, idx.to_ffi(), &mut isnum as *mut c_int);
            if isnum == 0 {
                Err(RunError::conversion_from_lua(self.type_at(idx), "f64", self.backtrace()))
            } else {
                Ok(num as f64)
            }
        }
    }

    fn to_integer(&mut self, idx: LuaIndex) -> RunResult<i64> {
        unsafe {
            let mut isnum: c_int = 0;
            let num = ffi::lua_tointegerx(self.lua, idx.to_ffi(), &mut isnum as *mut c_int);
            if isnum == 0 {
                Err(RunError::conversion_from_lua(self.type_at(idx), "i64", self.backtrace()))
            } else {
                Ok(num as i64)
            }
        }
    }

    fn to_boolean(&mut self, idx: LuaIndex) -> bool {
        unsafe { ffi::lua_toboolean(self.lua, idx.to_ffi()) != 0 }
    }

    fn to_string(&mut self, idx: LuaIndex) -> RunResult<String> {
        unsafe {
            let mut len: size_t = 0;
            let cstr = ffi::lua_tolstring(self.lua, idx.to_ffi(), &mut len as *mut size_t);
            let ty = self.type_at(idx);
            if cstr.is_null() {
                Err(RunError::conversion_from_lua(ty, "String", self.backtrace()))
            } else {
                use std::slice;
                Ok(try!(String::from_utf8(slice::from_raw_parts::<u8>(cstr as *const u8, len)
                        .to_vec())
                    .map_err(|_| RunError::conversion_from_lua(ty, "String", self.backtrace()))))
            }
        }
    }

    fn to_string_ptr(&mut self, idx: LuaIndex) -> RunResult<*const c_char> {
        unsafe {
            let ptr = ffi::lua_tostring(self.lua, idx.to_ffi());
            if ptr.is_null() {
                Err(RunError::conversion_from_lua(self.type_at(idx), "usize", self.backtrace()))
            } else {
                Ok(ptr)
            }
        }
    }

    fn to_unsigned(&mut self, idx: LuaIndex) -> RunResult<u64> {
        unsafe {
            let mut isnum: c_int = 0;
            let num = ffi::lua_tounsignedx(self.lua, idx.to_ffi(), &mut isnum as *mut c_int);
            if isnum == 0 {
                Err(RunError::conversion_from_lua(self.type_at(idx), "u64", self.backtrace()))
            } else {
                Ok(num as u64)
            }
        }
    }

    fn _to_thread(&mut self, _idx: i32) {
        unimplemented!();
        // unsafe { ffi::lua_tothread(self.lua, idx as c_int) }
    }

    fn _to_pointer(&mut self, _idx: i32) {
        unimplemented!();
        // unsafe { ffi::lua_topointer(self.lua, idx as c_int) }
    }

    pub fn _yield(&mut self, _nresults: i32) -> i32 {
        unimplemented!();
        // unsafe { ffi::lua_yield(self.lua, nresults as c_int) as i32 }
    }

    pub fn _to_native_function(&self, _idx: i32) -> Option<NativeFunction> {
        unimplemented!();
    }

    // Error
    fn lua_to_rust_load_result(&mut self, result: c_int) -> LoadResult<()> {
        match result {
            ffi::LUA_OK => Ok(()),
            ffi::LUA_ERRSYNTAX => Err(LoadError::Syntax(self.at(LuaIndex::Stack(-1)).unwrap())),
            ffi::LUA_ERRMEM => panic!("Lua memory allocation error"),
            ffi::LUA_ERRERR => panic!("Lua error handler failed"),
            _ => unreachable!("{}", result),
        }
    }

    fn lua_to_rust_run_result(&mut self, result: c_int) -> RunResult<()> {
        match result {
            ffi::LUA_OK => Ok(()),
            ffi::LUA_ERRRUN | ffi::LUA_ERRGCMM => {
                if self.is_userdata_of_type::<RunError>(LuaIndex::Stack(-1)) {
                    let err = self.userdata_move(LuaIndex::Stack(-1)).unwrap();
                    self.pop(1);
                    Err(err)
                } else if self.is_userdata_of_type::<Box<Any + Send>>(LuaIndex::Stack(-1)) {
                    let err = self.userdata_move(LuaIndex::Stack(-1)).unwrap();
                    self.pop(1);
                    panic::resume_unwind(err);
                } else {
                    unreachable!()
                }
            }
            ffi::LUA_ERRMEM => panic!("Lua memory allocation error"),
            ffi::LUA_ERRERR => panic!("Lua error handler failed"),
            _ => unreachable!("{}", result),
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            if self.should_free {
                ffi::lua_close(self.lua);
            }
        }
    }
}

// Miscellaneous private helper functions
fn rust_to_lua_op(op: LuaOperator) -> c_int {
    match op {
        LuaOperator::Add => ffi::LUA_OPADD,
        LuaOperator::Sub => ffi::LUA_OPSUB,
        LuaOperator::Mul => ffi::LUA_OPMUL,
        LuaOperator::Div => ffi::LUA_OPDIV,
        LuaOperator::IDiv => ffi::LUA_OPIDIV,
        LuaOperator::Mod => ffi::LUA_OPMOD,
        LuaOperator::Pow => ffi::LUA_OPPOW,
        LuaOperator::Unm => ffi::LUA_OPUNM,
        LuaOperator::BNot => ffi::LUA_OPBNOT,
        LuaOperator::BAnd => ffi::LUA_OPBAND,
        LuaOperator::BOr => ffi::LUA_OPBOR,
        LuaOperator::BXor => ffi::LUA_OPBXOR,
        LuaOperator::Shl => ffi::LUA_OPSHL,
        LuaOperator::Shr => ffi::LUA_OPSHR,
    }
}

fn lua_to_rust_type(typ: c_int) -> LuaType {
    match typ {
        ffi::LUA_TNIL => LuaType::Nil,
        ffi::LUA_TBOOLEAN => LuaType::Boolean,
        ffi::LUA_TNUMBER => LuaType::Number,
        ffi::LUA_TSTRING => LuaType::String,
        ffi::LUA_TFUNCTION => LuaType::Function,
        ffi::LUA_TUSERDATA => LuaType::Userdata,
        ffi::LUA_TLIGHTUSERDATA => LuaType::LightUserdata,
        ffi::LUA_TTHREAD => LuaType::Thread,
        ffi::LUA_TTABLE => LuaType::Table,
        _ => panic!("invalid Lua type"),
    }
}

fn lua_to_rust_type_checked(typ: c_int) -> Option<LuaType> {
    match typ {
        ffi::LUA_TNONE => None,
        _ => Some(lua_to_rust_type(typ)),
    }
}
