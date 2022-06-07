#![allow(unused_variables)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct RingBuffer<T: Send> {
    inner: Vec<T>,
    head: AtomicUsize,
    tail: Mutex<usize>,
    len: AtomicUsize,
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
            len: AtomicUsize::new(0),
            px_count: AtomicUsize::new(1),
            cx_count: AtomicBool::new(true),
        }
    }

    unsafe fn load(&self, pos: usize) -> T {
        (&self.inner[pos & (self.inner.len() - 1)] as *const T).read()
    }

    unsafe fn store(&self, pos: usize, val: T) {
        (&self.inner[pos & (self.inner.len() - 1)] as *const T as *mut T).write(val);
    }

    pub fn push_back(&self, val: T) -> Result<(), SendError<T>> {
        loop {
            let mut tail = self
                .tail
                .lock()
                .expect("failed to get the lock when push back to ring buffer");

            if self.is_full() {
                if !self.cx_exists() {
                    return Err(SendError(val));
                }

                continue;
            }

            unsafe { self.store(*tail, val) }
            *tail += 1;
            self.len.fetch_add(1, Ordering::AcqRel);

            return Ok(());
        }
    }

    pub fn pop_front(&self) -> Result<T, RecvError> {
        while self.is_empty() {
            let px_exist = self.px_exist();

            if !self.is_empty() {
                break;
            }

            if !px_exist {
                return Err(RecvError);
            }
        }

        let head = self.head.fetch_add(1, Ordering::AcqRel);
        let res = unsafe { self.load(head) };
        self.len.fetch_sub(1, Ordering::AcqRel);
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

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }
}

impl<T: Send> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        let head = self.head();
        for i in head..(head + self.len()) {
            drop(unsafe { self.load(i) });
        }

        unsafe { self.inner.set_len(0) }
    }
}

pub struct Producer<T: Send> {
    buf: Arc<RingBuffer<T>>,
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
        Self { buf }
    }

    pub fn send(&self, val: T) -> Result<(), SendError<T>> {
        self.buf.push_back(val)
    }
}

impl<T: Send> Clone for Producer<T> {
    fn clone(&self) -> Self {
        self.buf.px_count.fetch_add(1, Ordering::AcqRel);

        Self {
            buf: self.buf.clone(),
        }
    }
}

impl<T: Send> Drop for Producer<T> {
    fn drop(&mut self) {
        self.buf.px_count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl<T: Send> Consumer<T> {
    pub fn new(buf: Arc<RingBuffer<T>>) -> Self {
        Self { buf }
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        self.buf.pop_front()
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
    fn elements_arrive_ordered() {
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
