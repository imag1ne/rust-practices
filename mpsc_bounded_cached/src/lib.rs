#![allow(unused_variables)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::ptr::copy_nonoverlapping;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// the slice size that producer apply for.
const APPLY_SIZE: usize = 32;

pub struct RingBuffer<T: Send> {
    inner: Vec<Option<T>>,
    head: AtomicUsize,
    tail: Mutex<usize>,
    // right behind the last readable position.
    readable_till: AtomicUsize,
    len: AtomicUsize,
    reserved: AtomicUsize,
    // cap - len - reserved
    free: AtomicUsize,
    // slices that are filled and ready to be merged
    filled_slices: Mutex<BTreeSet<(usize, usize)>>,
    px_count: AtomicUsize,
    cx_count: AtomicBool,
}

impl<T: Send> RingBuffer<T> {
    pub fn new(cap: usize) -> Self {
        let cap = cap.next_power_of_two();
        let mut inner = Vec::with_capacity(cap);

        unsafe {
            inner.set_len(cap);
        }

        Self {
            inner,
            head: AtomicUsize::new(0),
            tail: Mutex::new(0),
            readable_till: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            reserved: AtomicUsize::new(0),
            free: AtomicUsize::new(cap),
            filled_slices: Mutex::new(BTreeSet::new()),
            px_count: AtomicUsize::new(1),
            cx_count: AtomicBool::new(true),
        }
    }

    unsafe fn load(&self, pos: usize) -> Option<T> {
        (&self.inner[pos & (self.inner.len() - 1)] as *const Option<T>).read()
    }

    unsafe fn store_vals(&self, pos: usize, mut vals: Vec<Option<T>>) {
        let cap = self.capacity();
        let vals_len = vals.len();
        let pos = pos & (cap - 1);
        let left = cap - pos;

        if vals_len > left {
            let src_ptr = vals.as_ptr() as *mut Option<T>;
            let dst_ptr = self.inner.as_ptr().add(pos) as *mut Option<T>;

            copy_nonoverlapping(src_ptr, dst_ptr, left);

            let src_ptr = vals.as_ptr().add(left) as *mut Option<T>;
            let dst_ptr = self.inner.as_ptr() as *mut Option<T>;
            copy_nonoverlapping(src_ptr, dst_ptr, vals_len - left);
        } else {
            let src_ptr = vals.as_ptr() as *mut Option<T>;
            let dst_ptr = self.inner.as_ptr().add(pos) as *mut Option<T>;

            copy_nonoverlapping(src_ptr, dst_ptr, vals_len);
        }

        vals.set_len(0);
    }

    pub fn pop_front(&self) -> Result<Option<T>, RecvError> {
        while self.is_empty() {
            let px_exist = self.px_exist();

            if !self.is_empty() {
                break;
            }

            if !px_exist {
                if self.reserved() > 0 {
                    self.merge_with_filled_slices(None);
                } else {
                    return Err(RecvError);
                }
            }
        }

        let head = self.head.fetch_add(1, Ordering::AcqRel);
        let res = unsafe { self.load(head) };
        self.len.fetch_sub(1, Ordering::AcqRel);
        self.free.fetch_add(1, Ordering::AcqRel);
        Ok(res)
    }

    pub fn head(&self) -> usize {
        self.head.load(Ordering::Acquire)
    }

    pub fn cx_exists(&self) -> bool {
        self.cx_count.load(Ordering::Acquire)
    }

    pub fn px_exist(&self) -> bool {
        self.px_count.load(Ordering::Acquire) > 0
    }

    pub fn capacity(&self) -> usize {
        self.inner.len()
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    pub fn reserved(&self) -> usize {
        self.reserved.load(Ordering::Acquire)
    }

    pub fn free(&self) -> usize {
        self.free.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        self.free() == 0
    }

    // apply for a slice on the ring buffer, the size parameter indicates the slice size that the producer want.
    // If the remaining space is not enough, the slice size is equal to the size of the remaining space.
    // The return value indicates the start position of the slice and its size.
    // If the ring buffer is full, return None.
    pub fn apply(&self, size: usize) -> Option<(usize, usize)> {
        let cap = self.capacity();
        let mut tail = self
            .tail
            .lock()
            .expect("failed to get the lock of tail when applying for new slice");
        let free = self.free();

        if free >= size {
            let start = *tail;
            *tail += size;

            self.reserved.fetch_add(size, Ordering::AcqRel);
            self.free.fetch_sub(size, Ordering::AcqRel);

            Some((start, size))
        } else if free > 0 {
            let start = *tail;
            *tail += free;

            self.reserved.fetch_add(free, Ordering::AcqRel);
            self.free.fetch_sub(free, Ordering::AcqRel);

            Some((start, free))
        } else {
            None
        }
    }

    // slices can be merged only if the start position of the slice is right behind the last readable position.
    pub fn merge_with_filled_slices(&self, mut filled_slice: Option<(usize, usize)>) {
        loop {
            let mut slices = self.filled_slices.lock().expect("failed to get the lock of filled_slices when merging the filled slices to ring buffer");
            if let Some(applied) = filled_slice.take() {
                slices.insert(applied);
            }

            if let Some((start, len)) = slices.iter().next().copied() {
                if self
                    .readable_till
                    .compare_exchange(start, start + len, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    slices.remove(&(start, len));
                    self.len.fetch_add(len, Ordering::AcqRel);
                    self.reserved.fetch_sub(len, Ordering::AcqRel);
                    // continue to merge.
                    continue;
                }
            }

            // break until there is no slice can be merged.
            break;
        }
    }
}

impl<T: Send> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        let head = self.head();
        let readable_till = self.readable_till.load(Ordering::Acquire);

        for i in head..readable_till {
            drop(unsafe { self.load(i) });
        }

        unsafe { self.inner.set_len(0) }
    }
}

pub struct Producer<T: Send> {
    buf: Arc<RingBuffer<T>>,
    buf_local: RefCell<Vec<Option<T>>>,
    allocated_slice: Cell<Option<(usize, usize)>>,
}

pub struct Consumer<T: Send> {
    buf: Arc<RingBuffer<T>>,
}

#[derive(Debug)]
pub struct SendError<T>(pub T);

#[derive(Debug)]
pub struct RecvError;

impl<T: Send> Producer<T> {
    pub fn new(buf: Arc<RingBuffer<T>>) -> Self {
        Self {
            buf,
            buf_local: RefCell::new(Vec::new()),
            allocated_slice: Cell::new(None),
        }
    }

    pub fn send(&self, val: T) -> Result<(), SendError<T>> {
        if let Some((start, len)) = self.allocated_slice.get() {
            self.buf_local.borrow_mut().push(Some(val));
            let buf_local_len = self.buf_local.borrow().len();

            if buf_local_len == len {
                self.flush();
            }
        } else {
            // apply first
            loop {
                if let Some(applied) = self.buf.apply(APPLY_SIZE) {
                    self.allocated_slice.set(Some(applied));
                    break;
                } else if !self.buf.cx_exists() {
                    return Err(SendError(val));
                }
            }

            self.send(val)?;
        }

        Ok(())
    }

    // writes all the data in local buffer to the channel's buffer, if the local buffer is not full,
    // then fill it using None.
    pub fn flush(&self) {
        match self.allocated_slice.get() {
            None => {}
            slice @ Some((start, len)) => {
                let buf_local_len = self.buf_local.borrow_mut().len();
                if buf_local_len < len {
                    let rest = len - buf_local_len;
                    self.buf_local.borrow_mut().extend((0..rest).map(|_| None));
                }
                let mut v = Vec::with_capacity(len);
                unsafe {
                    v.set_len(len);
                }

                self.buf_local.borrow_mut().swap_with_slice(&mut v);
                unsafe {
                    self.buf_local.borrow_mut().set_len(0);
                    self.buf.store_vals(start, v);
                }

                self.allocated_slice.set(None);
                self.buf.merge_with_filled_slices(slice);
            }
        }
    }
}

impl<T: Send> Clone for Producer<T> {
    fn clone(&self) -> Self {
        self.buf.px_count.fetch_add(1, Ordering::AcqRel);

        Self {
            buf: self.buf.clone(),
            buf_local: RefCell::new(Vec::new()),
            allocated_slice: Cell::new(None),
        }
    }
}

impl<T: Send> Drop for Producer<T> {
    fn drop(&mut self) {
        self.flush();
        self.buf.px_count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl<T: Send> Consumer<T> {
    pub fn new(buf: Arc<RingBuffer<T>>) -> Self {
        Self { buf }
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            if let Some(val) = self.buf.pop_front()? {
                return Ok(val);
            }
        }
    }
}

impl<T: Send> Drop for Consumer<T> {
    fn drop(&mut self) {
        self.buf.cx_count.store(false, Ordering::Release);
    }
}

impl<T: Send> Iterator for Consumer<T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        self.recv().ok()
    }
}

unsafe impl<T: Send> Send for Producer<T> {}
unsafe impl<T: Send> Send for Consumer<T> {}

pub fn channel<T: Send>() -> (Producer<T>, Consumer<T>) {
    let buf = Arc::new(RingBuffer::new(256));
    (Producer::new(buf.clone()), Consumer::new(buf))
}

// vorimplementierte Testsuite; bei Bedarf erweitern!

#[cfg(test)]
mod tests {
    use lazy_static::lazy_static;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::thread;

    use super::*;

    lazy_static! {
        static ref FOO_SET: Mutex<HashSet<i32>> = Mutex::new(HashSet::new());
    }

    #[derive(Debug)]
    struct Foo(i32);

    impl Foo {
        fn new(key: i32) -> Self {
            assert!(
                FOO_SET.lock().unwrap().insert(key),
                "double initialisation of element {}",
                key
            );
            Foo(key)
        }
    }

    impl Drop for Foo {
        fn drop(&mut self) {
            assert!(
                FOO_SET.lock().unwrap().remove(&self.0),
                "double free of element {}",
                self.0
            );
        }
    }

    // range of elements to be moved across the channel during testing
    const ELEMS: std::ops::Range<i32> = 0..1000;

    #[test]
    fn unused_elements_are_dropped() {
        lazy_static::initialize(&FOO_SET);

        for i in 0..100 {
            let (px, cx) = channel();
            let handle_1 = {
                let px = px.clone();
                thread::spawn(move || {
                    for i in 0.. {
                        if px.send(Foo::new(i)).is_err() {
                            return;
                        }
                    }
                })
            };

            let handle_2 = thread::spawn(move || {
                for i in 1.. {
                    if px.send(Foo::new(-i)).is_err() {
                        return;
                    }
                }
            });

            for _ in 0..i {
                cx.recv().unwrap();
            }

            drop(cx);

            assert!(handle_1.join().is_ok());
            assert!(handle_2.join().is_ok());

            let map = FOO_SET.lock().unwrap();
            if !map.is_empty() {
                panic!("FOO_MAP not empty: {:?}", *map);
            }
        }
    }

    #[test]
    fn elements_arrive_ordered_sp() {
        let (px, cx) = channel();

        thread::spawn(move || {
            for i in ELEMS {
                px.send(i).unwrap();
            }
        });

        for i in ELEMS {
            assert_eq!(i, cx.recv().unwrap());
        }

        assert!(cx.recv().is_err());
    }

    #[test]
    fn no_elements_lost() {
        for _ in 0..100 {
            let (px, cx) = channel();
            let handle = thread::spawn(move || {
                let mut count = 0;

                while let Ok(_) = cx.recv() {
                    count += 1;
                }

                count
            });

            {
                let px = px.clone();
                thread::spawn(move || {
                    for i in ELEMS {
                        px.send(i).unwrap();
                    }
                });
            }
            {
                let px = px.clone();
                thread::spawn(move || {
                    for i in ELEMS {
                        px.send(i + ELEMS.len() as i32).unwrap();
                    }
                });
            }

            thread::spawn(move || {
                for i in ELEMS {
                    px.send(i + (ELEMS.len() * 2) as i32).unwrap();
                }
            });

            match handle.join() {
                Ok(count) => assert_eq!(count, ELEMS.len() * 3),
                Err(_) => panic!("Error: join() returned Err"),
            }
        }
    }
}
