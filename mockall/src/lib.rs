// vim: tw=80
#![cfg_attr(feature = "nightly", feature(specialization))]

use cfg_if::cfg_if;
use downcast::*;
use std::{
    any,
    collections::hash_map::{DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    mem,
    ops::{DerefMut, Range},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering}
    }
};

pub use predicates::prelude::*;

trait AnyExpectation : Any + Send {}
downcast!(AnyExpectation);

trait ReturnDefault<O> {
    fn return_default() -> O;
}

/// Return functions for expectations
enum Rfunc<I, O> {
    Default,
    // Indicates that a `return_once` expectation has already returned
    Expired,
    Mut(Box<dyn FnMut(I) -> O + Send>),
    // Should be Box<dyn FnOnce> once that feature is stabilized
    // https://github.com/rust-lang/rust/issues/28796
    Once(Box<dyn FnMut(I) -> O + Send>),
}

// TODO: change this to "impl FnMut" once unboxed_closures are stable
// https://github.com/rust-lang/rust/issues/29625
impl<I, O>  Rfunc<I, O> {
    fn call_mut(&mut self, args: I) -> O {
        match self {
            Rfunc::Default => {
                Self::return_default()
            },
            Rfunc::Expired => {
                panic!("Called a method twice that was expected only once")
            },
            Rfunc::Mut(f) => {
                f(args)
            },
            Rfunc::Once(_) => {
                let fo = mem::replace(self, Rfunc::Expired);
                if let Rfunc::Once(mut f) = fo {
                    f(args)
                } else {
                    unreachable!()
                }
            },
        }
    }
}

impl<I, O> Default for Rfunc<I, O> {
    fn default() -> Self {
        Rfunc::Default
    }
}

cfg_if! {
    if #[cfg(feature = "nightly")] {
        impl<I, O> ReturnDefault<O> for Rfunc<I, O> {
            default fn return_default() -> O {
                panic!("Can only return default values for types that impl std::Default");
            }
        }

        impl<I, O: Default> ReturnDefault<O> for Rfunc<I, O> {
            fn return_default() -> O {
                O::default()
            }
        }
    } else {
        impl<I, O> ReturnDefault<O> for Rfunc<I, O> {
            fn return_default() -> O {
                panic!("Returning default values requires the \"nightly\" feature");
            }
        }
    }
}

struct Matcher<I>(Box<dyn Predicate<I> + Send>);

impl<I> Matcher<I> {
    fn verify(&self, i: &I) {
        if let Some(case) = self.0.find_case(false, &i) {
            let c = &case as &predicates_tree::CaseTreeExt;
            panic!("Expectation didn't match arguments:\n{}", c.tree());
        }
    }
}

impl<I> Default for Matcher<I> {
    fn default() -> Self {
        Matcher(Box::new(predicate::always()))
    }
}

struct Times{
    /// How many times has the expectation already been called?
    count: AtomicUsize,
    range: Range<usize>
}

impl Times {
    fn call(&self) {
        let count = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.range.end {
            if self.range.end == 0 {
                panic!("Expectation should not have been called");
            } else {
                let lim = self.range.end - 1;
                panic!("Expectation called more than {} times", lim);
            }
        }
    }

    fn any(&mut self) {
        self.range = 0..usize::max_value();
    }

    fn n(&mut self, n: usize) {
        self.range = n..(n+1);
    }

    fn never(&mut self) {
        self.range = 0..0;
    }

    fn range(&mut self, range: Range<usize>) {
        self.range = range;
    }
}

impl Default for Times {
    fn default() -> Self {
        // By default, allow any number of calls
        let count = AtomicUsize::default();
        let range = 0..usize::max_value();
        Times{count, range}
    }
}

impl Drop for Times {
    fn drop(&mut self) {
        let count = self.count.load(Ordering::Relaxed);
        if count < self.range.start {
            panic!("Expectation called fewer than {} times", self.range.start);
        }
    }
}

impl<I, O> Default for Expectation<I, O> {
    fn default() -> Self {
        Expectation::new()
    }
}

struct ExpectationCommon<I> {
    matcher: Matcher<I>,
    times: Times
}

impl<I> ExpectationCommon<I> {
    pub fn call(&self, i: &I) {
        self.matcher.verify(i);
        self.times.call();
    }

    /// Forbid this expectation from ever being called
    pub fn never(&mut self) {
        self.times.never();
    }

    /// Require this expectation to be called exactly `n` times.
    pub fn times(&mut self, n: usize) {
        self.times.n(n);
    }

    /// Allow this expectation to be called any number of times
    pub fn times_any(&mut self) {
        self.times.any();
    }

    /// Allow this expectation to be called any number of times within a given
    /// range
    pub fn times_range(&mut self, range: Range<usize>) {
        self.times.range(range);
    }

    pub fn with<P>(&mut self, p: P)
        where P: Predicate<I> + Send + 'static
    {
        self.matcher = Matcher(Box::new(p));
    }
}

impl<I> Default for ExpectationCommon<I> {
    fn default() -> Self {
        ExpectationCommon {
            times: Times::default(),
            matcher: Matcher::default()
        }
    }
}

/// Add common methods to Expectation types.  Must be called from within an
/// impl<I, O> {} block
macro_rules! expectation_common {
    ($common: ident, $klass:ident) => {
        /// Forbid this expectation from ever being called
        pub fn never(&mut self) -> &mut Self {
            self.$common.never();
            self
        }

        /// Require this expectation to be called exactly `n` times.
        pub fn times(&mut self, n: usize) -> &mut Self {
            self.$common.times(n);
            self
        }

        /// Allow this expectation to be called any number of times
        pub fn times_any(&mut self) -> &mut Self {
            self.$common.times_any();
            self
        }

        /// Allow this expectation to be called any number of times within a
        /// given range
        pub fn times_range(&mut self, range: Range<usize>) -> &mut Self {
            self.$common.times_range(range);
            self
        }

        pub fn with<P>(&mut self, p: P) -> &mut Self
            where P: Predicate<I> + Send + 'static
        {
            self.$common.with(p);
            self
        }
    }
}

/// Expectation type for methods that take return a `'static` type.
pub struct Expectation<I, O> {
    common: ExpectationCommon<I>,
    rfunc: Mutex<Rfunc<I, O>>,
}

impl<I, O> Expectation<I, O> {
    pub fn call(&self, i: I) -> O {
        self.common.call(&i);
        self.rfunc.lock().unwrap()
            .call_mut(i)
    }

    pub fn new() -> Self {
        let common = ExpectationCommon::default();
        let rfunc = Mutex::new(Rfunc::Default);
        Expectation{common, rfunc}
    }

    pub fn returning<F>(&mut self, f: F) -> &mut Self
        where F: FnMut(I) -> O + Send + 'static
    {
        {
            let mut guard = self.rfunc.lock().unwrap();
            mem::replace(guard.deref_mut(), Rfunc::Mut(Box::new(f)));
        }
        self
    }

    pub fn return_once<F>(&mut self, f: F) -> &mut Self
        where F: FnOnce(I) -> O + Send + 'static
    {
        let mut fopt = Some(f);
        let fmut = move |i| {
            if let Some(f) = fopt.take() {
                f(i)
            } else {
                panic!("Called a method twice that was expected only once")
            }
        };
        {
            let mut guard = self.rfunc.lock().unwrap();
            mem::replace(guard.deref_mut(), Rfunc::Once(Box::new(fmut)));
        }
        self
    }

    expectation_common!{common, Expectation}
}

impl<I: 'static, O: 'static> AnyExpectation for Expectation<I, O> {}

/// Expectation type for methods that take a `&self` argument and return a
/// reference with the same lifetime as `self`.
pub struct RefExpectation<I, O> {
    common: ExpectationCommon<I>,
    result: Option<O>,
    phantom: std::marker::PhantomData<I> ,
}

impl<I, O> RefExpectation<I, O> {
    pub fn call(&self, i: I) -> &O {
        self.common.call(&i);
        &self.result.as_ref()
            .expect("Must set return value with RefExpectation::return_const")
    }

    pub fn new() -> Self {
        let common = ExpectationCommon::default();
        let phantom = std::marker::PhantomData;
        RefExpectation{common, result: None, phantom}
    }

    pub fn return_const(&mut self, o: O) -> &mut Self {
        self.result = Some(o);
        self
    }

    expectation_common!{common, RefExpectation}
}

impl<I, O> Default for RefExpectation<I, O> {
    fn default() -> Self {
        RefExpectation::new()
    }
}

impl<I, O> AnyExpectation for RefExpectation<I, O>
    where I: Send + 'static, O: Send + 'static
{}

/// Expectation type for methods that take a `&mut self` argument and return a
/// mutable or immutable reference with the same lifetime as `self`.
pub struct RefMutExpectation<I, O> {
    common: ExpectationCommon<I>,
    result: Option<O>,
    rfunc: Option<Box<dyn FnMut(I) -> O + Send>>,
}

impl<I, O> RefMutExpectation<I, O> {
    pub fn call_mut(&mut self, i: I) -> &mut O {
        self.common.call(&i);
        if let Some(ref mut f) = self.rfunc {
            self.result = Some(f(i));
        }
        self.result.as_mut().expect("Must first set return function with RefMutExpectation::returning or return_var")
    }

    pub fn new() -> Self {
        let common = ExpectationCommon::default();
        RefMutExpectation{common, result: None, rfunc: None}
    }

    /// Convenience method that can be used to supply a return value for a
    /// `RefMutExpectation`.
    pub fn return_var(&mut self, o: O) -> &mut Self
    {
        self.result = Some(o);
        self
    }

    /// Supply a closure that the `RefMutExpectation` will use to create its
    /// return value.
    pub fn returning<F>(&mut self, f: F) -> &mut Self
        where F: FnMut(I) -> O + Send + 'static
    {
        mem::replace(&mut self.rfunc, Some(Box::new(f)));
        self
    }

    expectation_common!{common, RefMutExpectation}
}

impl<I, O> Default for RefMutExpectation<I, O> {
    fn default() -> Self {
        RefMutExpectation::new()
    }
}

impl<I, O> AnyExpectation for RefMutExpectation<I, O>
    where I: 'static, O: Send + 'static
{}

/// Non-generic keys to `GenericExpectations` internal storage
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Key(u64);

impl Key {
    fn new<I: 'static, O: 'static>(ident: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        ident.hash(&mut hasher);
        any::TypeId::of::<(I, O)>().hash(&mut hasher);
        Key(hasher.finish())
    }
}

#[derive(Default)]
pub struct GenericExpectations {
    store: HashMap<Key, Box<dyn AnyExpectation>>
}

impl GenericExpectations {
    pub fn expect<'e, I, O>(&'e mut self, ident: &str)
        -> &'e mut Expectation<I, O>
        where I: 'static, O: 'static
    {
        let key = Key::new::<I, O>(ident);
        let e = Box::new(Expectation::<I, O>::new());
        self.store.insert(key.clone(), e);
        self.store.get_mut(&key).unwrap()
            .downcast_mut()
            .unwrap()
    }

    // TODO: add a "called_nonstatic" method that can be used with types that
    // aren't 'static, and uses a different method to generate the key.
    pub fn call<I: 'static, O: 'static>(&self, ident: &str, args: I) -> O {
        let key = Key::new::<I, O>(ident);
        let e: &Expectation<I, O> = self.store.get(&key)
            .expect("No matching expectation found")
            .downcast_ref()
            .unwrap();
        e.call(args)
    }
}
