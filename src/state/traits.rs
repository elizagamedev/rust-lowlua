use std::convert::TryFrom;
use state::State;
use ::{Result, Error};

/// A conversion of a type into a Lua representation.
///
/// If successful, the type should leave a single value on the top of the Lua stack.
/// The top of the stack will be reset to the position it was before the function call on error,
/// so the `try!` macro will be safe in most cases, but this protection isn't sufficient to reset
/// if more advanced stack operations like `rotate()` are used.
/// Use caution when modifying the stack.
pub trait ToLua {
    fn to_lua(&self, state: &mut State) -> Result<()>;
}

/// A conversion from a Lua type on the stack into a native type.
///
/// The function should leave the stack as it found it, but like the `ToLua` trait, the state will
/// reset the top of the stack as a weak safety guarantee. Note that `from_lua()` should *not* remove
/// the original value from the stack.
pub trait FromLua: Sized {
    fn from_lua(state: &mut State, idx: i32) -> Result<Self>;
}

// Some standard implementations of the traits follow

// To
impl ToLua for u8 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushunsigned(*self as u64);
        Ok(())
    }
}

impl ToLua for u16 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushunsigned(*self as u64);
        Ok(())
    }
}

impl ToLua for u32 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushunsigned(*self as u64);
        Ok(())
    }
}

impl ToLua for u64 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushunsigned(*self);
        Ok(())
    }
}

impl ToLua for i8 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushinteger(*self as i64);
        Ok(())
    }
}

impl ToLua for i16 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushinteger(*self as i64);
        Ok(())
    }
}

impl ToLua for i32 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushinteger(*self as i64);
        Ok(())
    }
}

impl ToLua for i64 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushinteger(*self);
        Ok(())
    }
}

impl ToLua for f32 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushnumber(*self as f64);
        Ok(())
    }
}

impl ToLua for f64 {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushnumber(*self);
        Ok(())
    }
}

impl ToLua for bool {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushboolean(*self);
        Ok(())
    }
}

impl ToLua for str {
    fn to_lua(&self, state: &mut State) -> Result<()> {
        state.pushstring(self);
        Ok(())
    }
}

// From
impl FromLua for u8 {
    fn from_lua(state: &mut State, idx: i32) -> Result<u8> {
        Ok(try!(u8::try_from(try!(state.tounsigned(idx))).map_err(|_| Error::Type)))
    }
}

impl FromLua for u16 {
    fn from_lua(state: &mut State, idx: i32) -> Result<u16> {
        Ok(try!(u16::try_from(try!(state.tounsigned(idx))).map_err(|_| Error::Type)))
    }
}

impl FromLua for u32 {
    fn from_lua(state: &mut State, idx: i32) -> Result<u32> {
        Ok(try!(u32::try_from(try!(state.tounsigned(idx))).map_err(|_| Error::Type)))
    }
}

impl FromLua for u64 {
    fn from_lua(state: &mut State, idx: i32) -> Result<u64> {
        state.tounsigned(idx)
    }
}

impl FromLua for i8 {
    fn from_lua(state: &mut State, idx: i32) -> Result<i8> {
        Ok(try!(i8::try_from(try!(state.tointeger(idx))).map_err(|_| Error::Type)))
    }
}

impl FromLua for i16 {
    fn from_lua(state: &mut State, idx: i32) -> Result<i16> {
        Ok(try!(i16::try_from(try!(state.tointeger(idx))).map_err(|_| Error::Type)))
    }
}

impl FromLua for i32 {
    fn from_lua(state: &mut State, idx: i32) -> Result<i32> {
        Ok(try!(i32::try_from(try!(state.tointeger(idx))).map_err(|_| Error::Type)))
    }
}

impl FromLua for i64 {
    fn from_lua(state: &mut State, idx: i32) -> Result<i64> {
        state.tointeger(idx)
    }
}

impl FromLua for f32 {
    fn from_lua(state: &mut State, idx: i32) -> Result<f32> {
        // FIXME: f32 doesn't implement try_from
        Ok(try!(state.tonumber(idx)) as f32)
    }
}

impl FromLua for f64 {
    fn from_lua(state: &mut State, idx: i32) -> Result<f64> {
        state.tonumber(idx)
    }
}

impl FromLua for bool {
    fn from_lua(state: &mut State, idx: i32) -> Result<bool> {
        Ok(state.toboolean(idx))
    }
}

impl FromLua for String {
    fn from_lua(state: &mut State, idx: i32) -> Result<String> {
        state.tostring(idx)
    }
}
