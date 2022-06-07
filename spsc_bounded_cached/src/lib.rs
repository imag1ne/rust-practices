#![allow(unused_variables)]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

pub struct RingBuffer<T: Send> {
    inner: Vec<T>,
    head: AtomicUsize,
    tail: AtomicUsize,
    available: AtomicBool,
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
            tail: AtomicUsize::new(0),
            available: AtomicBool::new(true),
        }
    }

    unsafe fn load(&self, pos: usize) -> T {
        (&self.inner[pos & (self.inner.len() - 1)] as *const T).read()
    }

    unsafe fn store(&self, pos: usize, val: T) {
        (&self.inner[pos & (self.inner.len() - 1)] as *const T as *mut T).write(val);
    }

    pub fn head(&self) -> usize {
        self.head.load(Ordering::Acquire)
    }

    pub fn tail(&self) -> usize {
        self.tail.load(Ordering::Acquire)
    }

    pub fn available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    pub fn capacity(&self) -> usize {
        self.inner.len()
    }

    pub fn len(&self) -> usize {
        self.head() - self.tail()
    }
}

impl<T: Send> Drop for RingBuffer<T> {
    fn drop(&mut self) {
        for i in self.head()..self.tail() {
            drop(unsafe { self.load(i) });
        }

        unsafe { self.inner.set_len(0) }
    }
}

pub struct Producer<T: Send> {
    buf: Arc<RingBuffer<T>>,
    tail: Cell<usize>,
    free: Cell<usize>,
}

pub struct Consumer<T: Send> {
    buf: Arc<RingBuffer<T>>,
    head: Cell<usize>,
    full: Cell<usize>,
}

#[derive(Debug)]
pub struct SendError<T>(pub T);

#[derive(Debug)]
pub struct RecvError;

impl<T: Send> Producer<T> {
    pub fn new(buf: Arc<RingBuffer<T>>) -> Self {
        Self {
            tail: Cell::new(0),
            free: Cell::new(buf.capacity()),
            buf,
        }
    }

    pub fn send(&self, val: T) -> Result<(), SendError<T>> {
        while self.free.get() == 0 {
            if !self.buf.available() {
                return Err(SendError(val));
            }

            let cnt = self.tail.get() - self.buf.head();
            self.free.set(self.buf.capacity() - cnt);
        }

        unsafe {
            self.buf.store(self.tail.get(), val);
        }

        let new_tail = self.tail.get() + 1;
        self.tail.set(new_tail);
        self.buf.tail.store(new_tail, Ordering::Release);
        self.free.set(self.free.get() - 1);

        Ok(())
    }
}

impl<T: Send> Drop for Producer<T> {
    fn drop(&mut self) {
        self.buf.available.store(false, Ordering::Release);
    }
}

impl<T: Send> Consumer<T> {
    pub fn new(buf: Arc<RingBuffer<T>>) -> Self {
        Self {
            buf,
            head: Cell::new(0),
            full: Cell::new(0),
        }
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        if self.full.get() == 0 {
            loop {
                let available = self.buf.available();

                let cnt = self.buf.tail() - self.head.get();
                self.full.set(cnt);

                if self.full.get() > 0 {
                    break;
                }

                if !available {
                    return Err(RecvError);
                }
            }
        }

        let res = unsafe { self.buf.load(self.head.get()) };
        let new_head = self.head.get() + 1;
        self.head.set(new_head);
        self.buf.head.store(new_head, Ordering::Release);
        self.full.set(self.full.get() - 1);
        Ok(res)
    }
}

impl<T: Send> Drop for Consumer<T> {
    fn drop(&mut self) {
        self.buf.available.store(false, Ordering::Release);
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
            let handle = thread::spawn(move || {
                for i in 0.. {
                    if px.send(Foo::new(i)).is_err() {
                        return;
                    }
                }
            });

            for _ in 0..i {
                cx.recv().unwrap();
            }

            drop(cx);

            assert!(handle.join().is_ok());

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

            thread::spawn(move || {
                for i in ELEMS {
                    px.send(i).unwrap();
                }
            });

            match handle.join() {
                Ok(count) => assert_eq!(count, ELEMS.len()),
                Err(_) => panic!("Error: join() returned Err"),
            }
        }
    }
}
