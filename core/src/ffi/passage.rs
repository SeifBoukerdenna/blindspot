use super::*;

/// Reads one immutable indexed passage. Call on a worker; free with `bs_free_blob`.
/// Returns an empty blob when the row is stale, out of scope or unavailable.
///
/// # Safety
/// `handle` must be NULL or live for this call. `path` must be NULL or a valid
/// NUL-terminated UTF-8 string. Concurrent calls are supported.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_passage(handle:*const BsHandle,row_id:u64,path:*const c_char)->BsBlob {
    let empty=||BsBlob {data:std::ptr::null(),len:0};
    if handle.is_null() || path.is_null() {return empty();}
    // SAFETY: the caller keeps both pointers alive for the duration of this call.
    let (handle,path)=unsafe {(&*handle,CStr::from_ptr(path))};
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(path)=path.to_str() else {return empty();};
        if path.len()>4096 {return empty();}
        let chunk=((row_id ^ crate::index::fnv1a(&[path.as_bytes()])).rotate_right(17)) as i64;
        if chunk<=0 {return empty();}
        let Some(text)=handle.content.stored_passage(chunk,std::path::Path::new(path)) else {return empty();};
        let (data,len)=leak_bytes(text.as_bytes());
        BsBlob {data,len}
    })).unwrap_or_else(|_|empty())
}
