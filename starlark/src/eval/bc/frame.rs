/*
 * Copyright 2019 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Local variables and stack, in single allocation.

use std::{cell::Cell, mem, mem::MaybeUninit, ptr, slice};

use gazebo::dupe::Dupe;

use crate::{
    eval::{
        bc::{if_debug::IfDebug, stack_ptr::BcStackPtr},
        runtime::slots::LocalSlotId,
        Evaluator,
    },
    values::{Trace, Tracer, Value},
};

/// Current `def` frame (but not native function frame).
///
/// We erase lifetime here, because it is very hard to do lifetimes properly.
#[repr(C)]
struct BcFrame<'v> {
    /// Number of local slots.
    local_count: u32,
    /// Number of stack slots.
    max_stack_size: u32,
    /// Current stack pointer.
    stack_ptr_if_debug: IfDebug<*mut Value<'v>>,
    /// `local_count` local slots followed by `max_stack_size` stack slots.
    slots: [Option<Value<'v>>; 0],
}

#[derive(Copy, Clone, Dupe)]
pub(crate) struct BcFramePtr<'v> {
    /// Pointer to the `slots` field of `BcFrame`.
    ///
    /// We could store `BcFrame` pointer here, but since the most common
    /// data accessed is slots, storing `slots` pointer is slightly more efficient:
    /// no need to add a constant when accessing the field.
    slots_ptr: *mut Option<Value<'v>>,
}

impl<'v> BcFramePtr<'v> {
    pub(crate) fn null() -> BcFramePtr<'v> {
        BcFramePtr {
            slots_ptr: ptr::null_mut(),
        }
    }

    /// Is this frame allocated or constructed empty?
    pub(crate) fn is_inititalized(self) -> bool {
        !self.slots_ptr.is_null()
    }

    fn frame(&self) -> &BcFrame<'v> {
        debug_assert!(self.is_inititalized());
        unsafe {
            let frame = (self.slots_ptr as *mut u8).sub(BcFrame::offset_of_slots()) as *mut BcFrame;
            &*frame
        }
    }

    fn frame_mut(&mut self) -> &mut BcFrame<'v> {
        debug_assert!(self.is_inititalized());
        unsafe {
            let frame = (self.slots_ptr as *mut u8).sub(BcFrame::offset_of_slots()) as *mut BcFrame;
            &mut *frame
        }
    }

    #[inline(always)]
    pub(crate) fn get_slot(self, slot: LocalSlotId) -> Option<Value<'v>> {
        self.frame().get_slot(slot)
    }

    #[inline(always)]
    pub(crate) fn set_slot(mut self, slot: LocalSlotId, value: Value<'v>) {
        self.frame_mut().set_slot(slot, value)
    }

    pub(crate) fn max_stack_size(self) -> u32 {
        self.frame().max_stack_size
    }

    #[inline(always)]
    pub(crate) fn stack_bottom_ptr<'a>(self) -> BcStackPtr<'v, 'a> {
        self.frame().stack_bottom_ptr()
    }

    #[inline(always)]
    pub(crate) fn set_stack_ptr_if_debug(mut self, ptr: &BcStackPtr<'v, '_>) {
        self.frame_mut().stack_ptr_if_debug.set(ptr.ptr());
    }

    #[inline(always)]
    pub(crate) fn locals(&self) -> &[Cell<Option<Value<'v>>>] {
        self.frame().locals()
    }
}

impl<'v> BcFrame<'v> {
    fn offset_of_slots() -> usize {
        memoffset::offset_of!(BcFrame<'v>, slots)
    }

    fn frame_ptr(&mut self) -> BcFramePtr<'v> {
        unsafe {
            BcFramePtr {
                slots_ptr: (self as *mut _ as *mut u8).add(Self::offset_of_slots()) as *mut _,
            }
        }
    }

    #[inline(always)]
    fn locals(&self) -> &[Cell<Option<Value<'v>>>] {
        unsafe {
            slice::from_raw_parts(
                self.slots.as_ptr() as *const Cell<Option<Value>>,
                self.local_count as usize,
            )
        }
    }

    #[inline(always)]
    fn locals_mut(&mut self) -> &mut [Option<Value<'v>>] {
        unsafe { slice::from_raw_parts_mut(self.slots.as_mut_ptr(), self.local_count as usize) }
    }

    #[inline(always)]
    fn locals_uninit(&mut self) -> &mut [MaybeUninit<Option<Value<'v>>>] {
        unsafe {
            slice::from_raw_parts_mut(self.slots.as_mut_ptr() as *mut _, self.local_count as usize)
        }
    }

    #[inline(always)]
    fn stack_bottom_ptr<'a>(&self) -> BcStackPtr<'v, 'a> {
        unsafe {
            // Here we (incorrectly) drop lifetime of self.
            // We need to do it, because we need `&mut Evaluator`
            // after stack pointer is acquired.
            BcStackPtr::new(slice::from_raw_parts_mut(
                self.slots.as_ptr().add(self.local_count as usize) as *mut _,
                self.max_stack_size as usize,
            ))
        }
    }

    /// Assert there are no values on the stack.
    #[inline(always)]
    pub(crate) fn debug_assert_stack_size_if_zero(&self) {
        debug_assert!(*self.stack_ptr_if_debug.get_ref_if_debug() == self.stack_bottom_ptr().ptr());
    }

    /// Gets a local variable. Returns None to indicate the variable is not yet assigned.
    #[inline(always)]
    pub(crate) fn get_slot(&self, slot: LocalSlotId) -> Option<Value<'v>> {
        debug_assert!(slot.0 < self.local_count);
        unsafe { self.slots.as_ptr().add(slot.0 as usize).read() }
    }

    #[inline(always)]
    pub(crate) fn set_slot(&mut self, slot: LocalSlotId, value: Value<'v>) {
        debug_assert!(slot.0 < self.local_count);
        unsafe {
            self.slots
                .as_mut_ptr()
                .add(slot.0 as usize)
                .write(Some(value))
        }
    }
}

unsafe impl<'v> Trace<'v> for BcFrame<'v> {
    fn trace(&mut self, tracer: &Tracer<'v>) {
        self.locals_mut().trace(tracer);
        // Note this does not trace the stack.
        // GC can be performed only when the stack is empty.
        self.debug_assert_stack_size_if_zero();
    }
}

unsafe impl<'v> Trace<'v> for BcFramePtr<'v> {
    fn trace(&mut self, tracer: &Tracer<'v>) {
        self.frame_mut().trace(tracer);
    }
}

#[inline(always)]
fn alloca_raw<'v, 'a, R>(
    eval: &mut Evaluator<'v, 'a>,
    local_count: u32,
    max_stack_size: u32,
    k: impl FnOnce(&mut Evaluator<'v, 'a>, BcFramePtr<'v>) -> R,
) -> R {
    assert_eq!(mem::align_of::<BcFrame>() % mem::size_of::<usize>(), 0);
    assert_eq!(mem::size_of::<Value>(), mem::size_of::<usize>());
    let alloca_size_in_words = mem::size_of::<BcFrame>() / mem::size_of::<usize>()
        + (local_count as usize)
        + (max_stack_size as usize);
    eval.alloca_uninit::<usize, _, _>(alloca_size_in_words, |slice, eval| unsafe {
        let frame_ptr = slice.as_mut_ptr() as *mut BcFrame;
        *(frame_ptr) = BcFrame {
            local_count,
            max_stack_size,
            stack_ptr_if_debug: IfDebug::new(ptr::null_mut()),
            slots: [],
        };
        (*frame_ptr)
            .stack_ptr_if_debug
            .set((*frame_ptr).stack_bottom_ptr().ptr());

        k(eval, (*frame_ptr).frame_ptr())
    })
}

/// Allocate a frame and store it in the evaluator.
///
/// After callback finishes, previous frame is restored.
#[inline(always)]
pub(crate) fn alloca_frame<'v, 'a, R>(
    eval: &mut Evaluator<'v, 'a>,
    local_count: u32,
    max_stack_size: u32,
    k: impl FnOnce(&mut Evaluator<'v, 'a>) -> R,
) -> R {
    alloca_raw(eval, local_count, max_stack_size, |eval, mut frame| {
        // TODO(nga): no need to fill the slots for parameters.
        frame
            .frame_mut()
            .locals_uninit()
            .fill(MaybeUninit::new(None));
        let old_frame = mem::replace(&mut eval.current_frame, frame);
        let r = k(eval);
        eval.current_frame = old_frame;
        r
    })
}
