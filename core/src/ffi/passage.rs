use super::*;

/// Inspects one user-selected file without changing the index. Call on a worker.
/// Free the returned JSON with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or live; `path` must be NULL or valid NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_content_inspect(handle: *const BsHandle, path: *const c_char) -> BsBlob {
    if handle.is_null() || path.is_null() { return leak_blob(b""); }
    // SAFETY: the caller retains both pointers throughout this call.
    let (handle, path) = unsafe { (&*handle, CStr::from_ptr(path)) };
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(path) = path.to_str() else { return leak_blob(b""); };
        if path.len() > 4096 { return leak_blob(b""); }
        serde_json::to_vec(&handle.content.inspect_file(std::path::Path::new(path)))
            .map_or_else(|_| leak_blob(b""), |data| leak_blob(&data))
    })).unwrap_or_else(|_| leak_blob(b""))
}

/// Returns bounded indexing-control and model-health state; free with `bs_free_blob`.
/// # Safety
/// `handle` must be NULL or live for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_index_controls(handle: *const BsHandle) -> BsBlob {
    if handle.is_null() { return leak_blob(b""); }
    // SAFETY: the caller retains the handle throughout the call.
    let handle = unsafe { &*handle };
    catch_unwind(AssertUnwindSafe(|| {
        handle.content.recovery_tick();
        serde_json::to_vec(&handle.content.controls_state()).map_or_else(|_| leak_blob(b""), |data| leak_blob(&data))
    })).unwrap_or_else(|_| leak_blob(b""))
}

/// Enqueues pause/resume/retry/folder/check or a recovery tick. Empty blob means accepted.
/// # Safety
/// `handle` must be NULL or live; strings must be NULL or valid NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bs_index_action(handle: *const BsHandle, action: *const c_char, folder: *const c_char) -> BsBlob {
    if handle.is_null() || action.is_null() { return leak_blob(b"Index controls unavailable"); }
    // SAFETY: the caller retains the handle and strings throughout the call.
    let (handle, action) = unsafe { (&*handle, CStr::from_ptr(action)) };
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(action) = action.to_str() else { return leak_blob(b"Invalid indexing action"); };
        let folder = if folder.is_null() { "" } else {
            // SAFETY: the non-null string is retained by the caller.
            let Ok(value) = unsafe { CStr::from_ptr(folder) }.to_str() else { return leak_blob(b"Invalid folder"); };
            value
        };
        if action.len() > 16 || folder.len() > 4096 { return leak_blob(b"Invalid indexing action"); }
        match handle.content.index_action(action, folder) {
            Ok(()) => leak_blob(b""), Err(reason) => leak_blob(reason.as_bytes()),
        }
    })).unwrap_or_else(|_| leak_blob(b"Index control failed"))
}

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
