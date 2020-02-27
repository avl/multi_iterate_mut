use std::marker::PhantomData;
use crate::mypool3::{AuxHolder, AuxScheduler};

#[repr(align(64))]
pub(crate) struct PtrHolder1<'a, T> {
    t: usize,
    //*mut T
    chunksize: usize,
    pd: PhantomData<&'a T>,
}
impl<'a, T: Send + Sync> AuxHolder for PtrHolder1<'a, T> {
    unsafe fn cheap_copy(&self) -> Self {
        PtrHolder1 {
            t: self.t,
            chunksize: self.chunksize,
            pd: PhantomData,
        }
    }
}
impl<'a, T: Send + Sync> PtrHolder1<'a, T> {
    #[inline]
    pub fn get0(&self, index: usize) -> &mut T {
        unsafe { &mut *(self.t as *mut T).wrapping_add(index) }
        //println!("Get item {} returns {:?} from ptr {:?}",index, retp as *const T,self.t as *mut T);
    }
    #[inline]
    pub fn new(input: &'a mut [T], thread_count: usize) -> PtrHolder1<T> {
        PtrHolder1 {
            t: input.as_mut_ptr() as usize,
            chunksize: (input.len() + thread_count - 1) / thread_count,
            pd: PhantomData,
        }
    }
    #[inline]
    fn schedule0<'b, F: FnOnce(&mut T) + Sync + Send + 'b>(&self, ctx: &'b mut AuxScheduler<PtrHolder1<'a, T>>, index: usize, f: F)
    {
        ctx.schedule((index as usize / self.chunksize) as u32, move |auxitem| {
            f((auxitem).get0(index))
        })
    }
}
pub struct Context1<'a, 'b, A0> {
    pub(crate) ptr_holder: &'b mut AuxScheduler<'a, PtrHolder1<'a, A0>>
}
impl<'a, 'b, A0: Send + Sync> Context1<'a, 'b, A0> {
    pub fn schedule<F: FnOnce(&mut A0) + Send + Sync + 'b>(&mut self, index: usize, f: F) {
        self.ptr_holder.get_ah().schedule0(self.ptr_holder, index, f)
    }
}
