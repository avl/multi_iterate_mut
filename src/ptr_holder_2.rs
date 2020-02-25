use std::marker::PhantomData;
use crate::mypool3::{AuxHolder, AuxScheduler};

pub(crate) struct PtrHolder2<'a, T0, T1> {
    t0: usize,
    t1: usize,
    chunksize0: usize,
    chunksize1: usize,
    pd: PhantomData<&'a (T0,T1)>,
}
impl<'a, T0: Send + Sync,T1:Send+Sync> AuxHolder for PtrHolder2<'a, T0, T1> {
    unsafe fn cheap_copy(&self) -> Self {
        PtrHolder2 {
            t0: self.t0,
            t1: self.t1,
            chunksize0: self.chunksize0,
            chunksize1: self.chunksize1,
            pd: PhantomData,
        }
    }
}
impl<'a, T0: Send + Sync, T1: Send + Sync> PtrHolder2<'a, T0, T1> {
    #[inline]
    pub fn get0(&self, index: usize) -> &mut T0 {
        unsafe { &mut *(self.t0 as *mut T0).wrapping_add(index) }
    }
    pub fn get1(&self, index: usize) -> &mut T1 {
        unsafe { &mut *(self.t1 as *mut T1).wrapping_add(index) }
    }
    #[inline]
    pub fn new(input0: &'a mut [T0], input1: &'a mut [T1], thread_count: usize) -> PtrHolder2<'a,T0,T1> {
        PtrHolder2 {
            t0: input0.as_mut_ptr() as usize,
            t1: input1.as_mut_ptr() as usize,
            chunksize0: (input0.len() + thread_count - 1) / thread_count,
            chunksize1: (input1.len() + thread_count - 1) / thread_count,
            pd: PhantomData,
        }
    }
    fn schedule0<'b, F: FnOnce(&mut T0) + Sync + Send + 'b>(&self, ctx: &'b mut AuxScheduler<PtrHolder2<'a, T0,T1>>, index0: usize, f: F)
    {
        ctx.schedule((index0 as usize / self.chunksize0) as u32, move |auxitem0| {
            f((auxitem0).get0(index0))
        })
    }
    fn schedule1<'b, F: FnOnce(&mut T1) + Sync + Send + 'b>(&self, ctx: &'b mut AuxScheduler<PtrHolder2<'a, T0, T1>>, index1: usize, f: F)
    {
        ctx.schedule((index1 as usize / self.chunksize1) as u32, move |auxitem1| {
            f((auxitem1).get1(index1))
        })
    }
}
pub struct Context2<'a, 'b, T0,T1> {
    pub(crate) ptr_holder: &'b mut AuxScheduler<'a, PtrHolder2<'a, T0, T1>>
}
impl<'a, 'b, T0: Send + Sync, T1:Send+Sync> Context2<'a, 'b, T0, T1> {
    pub fn schedule0<F: FnOnce(&mut T0) + Send + Sync + 'b>(&mut self, index0: usize, f: F) {
        self.ptr_holder.get_ah().schedule0(self.ptr_holder, index0, f)
    }
    pub fn schedule1<F: FnOnce(&mut T1) + Send + Sync + 'b>(&mut self, index1: usize, f: F) {
        self.ptr_holder.get_ah().schedule1(self.ptr_holder, index1, f)
    }
}
