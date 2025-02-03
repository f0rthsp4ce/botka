use std::fmt::Debug;
use std::sync::Arc;

use dptree::di::Injectable;
use dptree::{from_fn_with_description, Handler, HandlerDescription};

/// Extension trait for [`dptree::Handler`].
pub trait HandlerExt<'a, Input, Output, Descr>: Sized + 'a
where
    Input: Send + 'a,
    Output: 'a,
    Descr: HandlerDescription,
{
    /// Similar to [`Handler::inspect_async`], but the handler function
    /// can return an error. The error will be logged then discarded.
    fn inspect_err<F, Error, Args>(
        self,
        f: F,
    ) -> Handler<'a, Input, Output, Descr>
    where
        F: Injectable<Input, Result<(), Error>, Args> + Send + Sync + 'a,
        Error: 'a + Debug;
}

impl<'a, Input, Output, Descr> HandlerExt<'a, Input, Output, Descr>
    for Handler<'a, Input, Output, Descr>
where
    Input: Send + 'a,
    Output: 'a,
    Descr: HandlerDescription,
{
    fn inspect_err<F, Error, Args>(self, f: F) -> Self
    where
        F: Injectable<Input, Result<(), Error>, Args> + Send + Sync + 'a,
        Error: 'a + Debug,
    {
        self.chain(inspect_err(f))
    }
}

pub fn inspect_err<'a, Input, Output, Error, Descr, F, Args>(
    f: F,
) -> Handler<'a, Input, Output, Descr>
where
    F: Injectable<Input, Result<(), Error>, Args> + Send + Sync + 'a,
    Input: Send + 'a,
    Error: Debug,
    Output: 'a,
    Descr: HandlerDescription,
{
    let f = Arc::new(f);
    from_fn_with_description(Descr::inspect_async(), move |x, cont| {
        let f: Arc<F> = Arc::clone(&f);
        async move {
            {
                let f = f.inject(&x);
                if let Err(e) = f().await {
                    log::error!("Error handling message: {e:?}");
                }
            }
            cont(x).await
        }
    })
}
