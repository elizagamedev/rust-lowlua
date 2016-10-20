use std::convert::TryFrom;
use state::State;
use ::{RunResult, RunError, LuaIndex, LuaString};

/// A conversion of a type into a Lua representation.
///
/// If successful, the type should leave a single value on the top of the Lua stack.
/// The top of the stack will be reset to the position it was before the function call on error,
/// so the `try!` macro will be safe in most cases, but this protection isn't sufficient to reset
/// if more advanced stack operations like `rotate()` are used.
/// Use caution when modifying the stack.
pub trait ToLua {
    fn to_lua(&self, state: &mut State);
}

/// A conversion from a Lua type on the stack into a native type.
///
/// The function should leave the stack as it found it, but like the `ToLua` trait, the state will
/// reset the top of the stack as a weak safety guarantee. Note that `from_lua()` should *not*
/// remove the original value from the stack.
pub trait FromLua: Sized {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<Self>;
}

// Some standard implementations of the traits follow

// To
impl ToLua for u8 {
    fn to_lua(&self, state: &mut State) {
        state.push_unsigned(*self as u64);
    }
}

impl ToLua for u16 {
    fn to_lua(&self, state: &mut State) {
        state.push_unsigned(*self as u64);
    }
}

impl ToLua for u32 {
    fn to_lua(&self, state: &mut State) {
        state.push_unsigned(*self as u64);
    }
}

impl ToLua for u64 {
    fn to_lua(&self, state: &mut State) {
        state.push_unsigned(*self);
    }
}

impl ToLua for usize {
    fn to_lua(&self, state: &mut State) {
        state.push_unsigned(*self as u64);
    }
}

impl ToLua for i8 {
    fn to_lua(&self, state: &mut State) {
        state.push_integer(*self as i64);
    }
}

impl ToLua for i16 {
    fn to_lua(&self, state: &mut State) {
        state.push_integer(*self as i64);
    }
}

impl ToLua for i32 {
    fn to_lua(&self, state: &mut State) {
        state.push_integer(*self as i64);
    }
}

impl ToLua for i64 {
    fn to_lua(&self, state: &mut State) {
        state.push_integer(*self);
    }
}

impl ToLua for isize {
    fn to_lua(&self, state: &mut State) {
        state.push_integer(*self as i64);
    }
}

impl ToLua for f32 {
    fn to_lua(&self, state: &mut State) {
        state.push_number(*self as f64);
    }
}

impl ToLua for f64 {
    fn to_lua(&self, state: &mut State) {
        state.push_number(*self);
    }
}

impl ToLua for bool {
    fn to_lua(&self, state: &mut State) {
        state.push_boolean(*self);
    }
}

impl<'a> ToLua for &'a str {
    fn to_lua(&self, state: &mut State) {
        state.push_string(self);
    }
}

impl<'a> ToLua for String {
    fn to_lua(&self, state: &mut State) {
        state.push_string(self);
    }
}

impl ToLua for LuaString {
    fn to_lua(&self, state: &mut State) {
        state.get_internal_registry();
        state.get_field(LuaIndex::Stack(-1), "string");
        state.remove(-2);
        state.push(self.0);
        state.raw_get(LuaIndex::Stack(-2));
        state.remove(-2);
    }
}

// From
impl FromLua for u8 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<u8> {
        Ok(try!(u8::try_from(try!(state.to_unsigned(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "u8"))))
    }
}

impl FromLua for u16 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<u16> {
        Ok(try!(u16::try_from(try!(state.to_unsigned(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "u16"))))
    }
}

impl FromLua for u32 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<u32> {
        Ok(try!(u32::try_from(try!(state.to_unsigned(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "u32"))))
    }
}

impl FromLua for u64 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<u64> {
        state.to_unsigned(idx)
    }
}

impl FromLua for usize {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<usize> {
        Ok(try!(usize::try_from(try!(state.to_unsigned(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "usize"))))
    }
}

impl FromLua for i8 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<i8> {
        Ok(try!(i8::try_from(try!(state.to_integer(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "i8"))))
    }
}

impl FromLua for i16 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<i16> {
        Ok(try!(i16::try_from(try!(state.to_integer(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "i16"))))
    }
}

impl FromLua for i32 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<i32> {
        Ok(try!(i32::try_from(try!(state.to_integer(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "i32"))))
    }
}

impl FromLua for i64 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<i64> {
        state.to_integer(idx)
    }
}

impl FromLua for isize {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<isize> {
        Ok(try!(isize::try_from(try!(state.to_integer(idx)))
            .map_err(|_| RunError::conversion_from_lua(state.type_at(idx), "isize"))))
    }
}

impl FromLua for f32 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<f32> {
        // FIXME: f32 doesn't implement try_from
        Ok(try!(state.to_number(idx)) as f32)
    }
}

impl FromLua for f64 {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<f64> {
        state.to_number(idx)
    }
}

impl FromLua for bool {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<bool> {
        Ok(state.to_boolean(idx))
    }
}

impl FromLua for String {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<String> {
        state.to_string(idx)
    }
}

impl FromLua for LuaString {
    fn from_lua(state: &mut State, idx: LuaIndex) -> RunResult<LuaString> {
        let val = try!(state.to_string_ptr(idx)) as usize;
        // Record a reference to the string
        state.get_internal_registry();
        state.get_field(LuaIndex::Stack(-1), "string");
        state.push(val);
        state.push_value(idx);
        state.raw_set(LuaIndex::Stack(-3));
        state.pop(2);
        Ok(LuaString(val))
    }
}
