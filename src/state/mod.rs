mod traits;

use ffi;
use libc;

use std::{io, ptr};
use std::ffi::CString;
use std::mem::transmute;
use libc::{c_int, c_char, size_t, c_void};

use super::{Result, Error, LuaType, LuaOperator, LuaCallResults, LuaIndex, NativeFunction};

pub use self::traits::*;

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

        // Set the panic handler
        unsafe { ffi::lua_atpanic(lua, panic) };
        extern "C" fn panic(lua: *mut ffi::lua_State) -> libc::c_int {
            let mut state = State::from_raw_state(lua);
            let err = state.to_string(-1).unwrap();
            panic!("PANIC: unprotected error in call to Lua API ({})", err);
        }

        // Create the state object
        let mut state = State {
            lua: lua,
            should_free: true,
        };

        // Add the address of the State object to the "extradata" and create a table in the
        // registry. The address isn't actually used to track the location of the object, but the
        // usage of an address for the key of a registry table was recommended by the Lua 5.3
        // reference manual.
        unsafe {
            let extraspace = transmute::<*mut c_void,
                                         *mut *mut c_void>(ffi::lua_getextraspace(state.lua));
            *extraspace = transmute::<*mut State, *mut c_void>(&mut state as *mut State);
            ffi::lua_newtable(state.lua);
            ffi::lua_rawsetp(state.lua, ffi::LUA_REGISTRYINDEX, *extraspace);
        }
        state
    }

    /// Opens all standard Lua libraries into the state.
    pub fn open_libs(&mut self) {
        unsafe { ffi::luaL_openlibs(self.lua) }
    }

    /// Load string containing Lua code as a Lua function on the top of the stack.
    /// If an error occurs, nothing is pushed to the stack.
    pub fn load_string(&mut self, str: &str, chunkname: &str) -> Result<()> {
        // TODO: special case for strings so there's not so much memory movement?
        let vec = str.as_bytes().to_vec();
        self.load_stream(vec.as_slice(), chunkname)
    }

    /// Load string containing Lua code as a Lua function on the top of the stack.
    /// If an error occurs, nothing is pushed to the stack.
    pub fn load_stream<R: io::Read>(&mut self, stream: R, chunkname: &str) -> Result<()> {
        extern "C" fn reader<R: io::Read>(_lua: *mut ffi::lua_State,
                                          data: *mut c_void,
                                          size: *mut size_t)
                                          -> *const c_char {
            unsafe {
                let ref mut rd = *transmute::<*mut c_void, *mut ReaderData<R>>(data);
                rd.string.truncate(0);
                rd.stream.read_to_end(&mut rd.string).unwrap();
                *size = rd.string.len() as size_t;
                transmute::<*const u8, *const c_char>(rd.string.as_ptr())
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
                          transmute::<*mut ReaderData<R>,
                                      *mut c_void>(&mut data as *mut ReaderData<R>),
                          CString::new(chunkname).unwrap().as_ptr(),
                          ptr::null())
        };
        lua_to_rust_result(result)
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
    pub fn call(&mut self, nargs: u32, results: LuaCallResults) -> Result<()> {
        let nresults = match results {
            LuaCallResults::Num(val) => val as c_int,
            LuaCallResults::MultRet => ffi::LUA_MULTRET,
        };
        let result = unsafe { ffi::lua_pcall(self.lua, nargs as c_int, nresults, 0) };
        lua_to_rust_result(result)
    }

    /// Push a type on the top of the stack.
    pub fn push<T: ToLua>(&mut self, val: T) -> Result<()> {
        let top = self.get_top();
        let result = val.to_lua(self);
        if result.is_err() {
            self.set_top(top)
        }
        result
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
                let f = *transmute::<*mut c_void, *mut NativeFunction>(ffi::lua_touserdata(lua, ffi::lua_upvalueindex(1)));
                let mut state = State::from_raw_state(lua);
                f(&mut state) as c_int
            }
        }

        unsafe {
            // Push userdata instead of light userdata, as some platforms may have differing
            // pointer sizes between functions and variables.
            use std::mem::size_of;
            let ud =
                transmute::<*mut c_void,
                            *mut NativeFunction>(ffi::lua_newuserdata(self.lua,
                                                                      size_of::<NativeFunction>()));
            *ud = f;
            let n = (n + 1) as i32;
            if n > 1 {
                self.insert(-n);
            }
            ffi::lua_pushcclosure(self.lua, func, n);
        }
    }

    /// This function allocates a new block of memory with the size and contents of `data`,
    /// pushes onto the stack a new full userdata with the block address, and returns a slice to
    /// the block. The host program can freely use this memory.
    pub fn push_userdata(&mut self, data: &[u8]) -> &mut [u8] {
        unsafe {
            use std::slice;
            let ptr = ffi::lua_newuserdata(self.lua, data.len() as size_t);
            libc::memcpy(ptr,
                         transmute::<*const u8, *const c_void>(data.as_ptr()),
                         data.len());
            slice::from_raw_parts_mut(transmute::<*mut c_void, *mut u8>(ptr), data.len())
        }
    }

    /// Pushes a light userdata onto the stack.
    ///
    /// Userdata represent native values in Lua. A light userdata represents a pointer to memory.
    /// It is a value (like a number): you do not create it, it has no individual metatable, and it
    /// is not collected (as it was never created). A light userdata is equal to "any" light
    /// userdata with the same memory address.
    pub fn push_light_userdata<T>(&mut self, p: *const T) {
        unsafe {
            let p = transmute::<*const T, *mut c_void>(p);
            ffi::lua_pushlightuserdata(self.lua, p);
        }
    }

    /// Pushes a `nil` value onto the stack.
    pub fn push_nil(&mut self) {
        unsafe { ffi::lua_pushnil(self.lua) }
    }

    /// Get a type from a place on the stack.
    pub fn at<T: FromLua>(&mut self, idx: LuaIndex) -> Result<T> {
        let top = self.get_top();
        let result = T::from_lua(self, idx.to_ffi());
        self.set_top(top);
        result
    }

    /// Returns the slice of data represented by the userdata at the given index if the value
    /// is userdata. This function does not work with light userdata.
    pub fn userdata_at(&self, idx: LuaIndex) -> Result<&mut [u8]> {
        unsafe {
            if ffi::lua_type(self.lua, idx.to_ffi()) == ffi::LUA_TUSERDATA {
                use std::slice;
                let len = ffi::lua_rawlen(self.lua, idx.to_ffi()) as usize;
                let ptr = transmute::<*mut c_void, *mut u8>(ffi::lua_touserdata(self.lua,
                                                                                idx.to_ffi()));
                Ok(slice::from_raw_parts_mut(ptr, len))
            } else {
                Err(Error::Type)
            }
        }
    }

    /// Returns the pointer represented by the light userdata at the given index if the type is
    /// light userdata.
    pub fn light_userdata_at<T>(&self, idx: LuaIndex) -> Result<*mut T> {
        unsafe {
            if ffi::lua_type(self.lua, idx.to_ffi()) == ffi::LUA_TLIGHTUSERDATA {
                Ok(transmute::<*mut c_void, *mut T>(ffi::lua_touserdata(self.lua, idx.to_ffi())))
            } else {
                Err(Error::Type)
            }
        }
    }

    /// Converts the acceptable index idx into an equivalent absolute index (that is, one that does
    /// not depend on the stack top).
    pub fn abs_index(&self, idx: i32) -> i32 {
        unsafe { ffi::lua_absindex(self.lua, idx as c_int) as i32 }
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
    pub fn raw_get_i(&mut self, idx: LuaIndex, n: i32) -> LuaType {
        lua_to_rust_type(unsafe { ffi::lua_rawgeti(self.lua, idx.to_ffi(), n as c_int) })
    }

    /// Pushes onto the stack the value `t[k]`, where `t` is the table at the given index and
    /// `k` is the pointer `p` represented as a light userdata. The access is raw; that is,
    /// it does not invoke the `__index` metamethod.
    ///
    /// Returns the type of the pushed value.
    pub fn raw_get_p<T>(&mut self, idx: LuaIndex, p: *const T) -> LuaType {
        unsafe {
            let p = transmute::<*const T, *const c_void>(p);
            lua_to_rust_type(ffi::lua_rawgetp(self.lua, idx.to_ffi(), p))
        }
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

    /// Pushes onto the stack the Lua value associated with the userdata at the given index.
    ///
    /// Returns the type of the pushed value.
    pub fn get_user_value(&mut self, idx: LuaIndex) -> LuaType {
        lua_to_rust_type(unsafe { ffi::lua_getuservalue(self.lua, idx.to_ffi()) })
    }

    /// Pushes our registry table onto the stack. When the Lua state is crated, a table is
    /// automatically allocated in the registry that can be freely used by the program.
    pub fn get_registry(&mut self) {
        unsafe {
            let extraspace = transmute::<*mut c_void,
                                         *const *mut c_void>(ffi::lua_getextraspace(self.lua));
            ffi::lua_rawgetp(self.lua, ffi::LUA_REGISTRYINDEX, *extraspace);
        }
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
    pub fn raw_set_i(&mut self, idx: LuaIndex, n: i32) {
        unsafe { ffi::lua_rawseti(self.lua, idx.to_ffi(), n as c_int) }
    }

    /// Does the equivalent of `t[p] = v`, where `t` is the table at the given index,
    /// `p` is encoded as a light userdata, and `v` is the value at the top of the stack.
    ///
    /// This function pops the value from the stack. The assignment is raw, that is,
    /// it does not invoke the `__newindex` metamethod.
    pub fn raw_set_p<T>(&mut self, idx: LuaIndex, p: *const T) {
        unsafe {
            let p = transmute::<*const T, *const c_void>(p);
            ffi::lua_rawsetp(self.lua, idx.to_ffi(), p);
        }
    }

    /// Pops a table from the stack and sets it as the new metatable for the value at the
    /// given index.
    pub fn set_metatable(&mut self, objindex: LuaIndex) {
        unsafe { ffi::lua_setmetatable(self.lua, objindex.to_ffi()) };
    }

    /// Pops a value from the stack and sets it as the new value associated to the userdata at the
    /// given index.
    pub fn set_user_value(&mut self, idx: LuaIndex) {
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



    // Internal

    // This function is needed because it's unfortunately not possible to guarantee a reference
    // to the `State` that called a native function without making State boxed or something.
    fn from_raw_state(state: *mut ffi::lua_State) -> State {
        State {
            lua: state,
            should_free: false,
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
        unsafe {
            let cstr = transmute::<*const u8, *const c_char>(s.as_ptr());
            ffi::lua_pushlstring(self.lua, cstr, s.len() as size_t)
        }
    }

    fn push_boolean(&mut self, b: bool) {
        unsafe { ffi::lua_pushboolean(self.lua, if b { 1 } else { 0 }) }
    }

    fn push_unsigned(&mut self, n: u64) {
        unsafe { ffi::lua_pushunsigned(self.lua, n as ffi::lua_Unsigned) }
    }

    // To

    fn to_number(&mut self, idx: i32) -> Result<f64> {
        unsafe {
            let mut isnum: c_int = 0;
            let num = ffi::lua_tonumberx(self.lua, idx as c_int, &mut isnum as *mut c_int);
            if isnum == 0 {
                Err(Error::Type)
            } else {
                Ok(num as f64)
            }
        }
    }

    fn to_integer(&mut self, idx: i32) -> Result<i64> {
        unsafe {
            let mut isnum: c_int = 0;
            let num = ffi::lua_tointegerx(self.lua, idx as c_int, &mut isnum as *mut c_int);
            if isnum == 0 {
                Err(Error::Type)
            } else {
                Ok(num as i64)
            }
        }
    }

    fn to_boolean(&mut self, idx: i32) -> bool {
        unsafe { ffi::lua_toboolean(self.lua, idx as c_int) != 0 }
    }

    fn to_string(&mut self, idx: i32) -> Result<String> {
        unsafe {
            let mut len: size_t = 0;
            let cstr = ffi::lua_tolstring(self.lua, idx as c_int, &mut len as *mut size_t);
            if cstr.is_null() {
                Err(Error::Type)
            } else {
                use std::slice;
                let cstr = transmute::<*const c_char, *const u8>(cstr);
                Ok(try!(String::from_utf8(slice::from_raw_parts::<u8>(cstr, len).to_vec())))
            }
        }
    }

    fn to_unsigned(&mut self, idx: i32) -> Result<u64> {
        unsafe {
            let mut isnum: c_int = 0;
            let num = ffi::lua_tounsignedx(self.lua, idx as c_int, &mut isnum as *mut c_int);
            if isnum == 0 {
                Err(Error::Type)
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

fn lua_to_rust_result(result: c_int) -> Result<()> {
    match result {
        ffi::LUA_OK => Ok(()),
        ffi::LUA_ERRSYNTAX => Err(Error::Syntax),
        ffi::LUA_ERRGCMM => Err(Error::GcMetamethod),
        ffi::LUA_ERRRUN => Err(Error::Runtime),
        ffi::LUA_ERRMEM => panic!("Lua memory allocation error"),
        _ => unreachable!(),
    }
}
