// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Routes Finder document-open and browser install events to the existing package-review flow.

use std::ffi::CStr;

use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{msg_send, sel, MainThreadMarker};
use objc2_app_kit::NSApplication;

extern "C-unwind" fn open_files(
    _delegate: &AnyObject,
    _selector: Sel,
    application: &AnyObject,
    files: &AnyObject,
) {
    // SAFETY: AppKit supplies an NSArray of NSString file paths for this selector.
    unsafe {
        let count: usize = msg_send![files, count];
        for index in 0..count {
            let file: *const AnyObject = msg_send![files, objectAtIndex: index];
            forward_string(&*file);
        }
        let _: () = msg_send![application, replyToOpenOrPrint: 0usize];
    }
}

extern "C-unwind" fn open_urls(
    _delegate: &AnyObject,
    _selector: Sel,
    _application: &AnyObject,
    urls: &AnyObject,
) {
    // SAFETY: AppKit supplies an NSArray of NSURLs retained for this callback.
    unsafe {
        let count: usize = msg_send![urls, count];
        for index in 0..count {
            let url: *const AnyObject = msg_send![urls, objectAtIndex: index];
            let file: bool = msg_send![&*url, isFileURL];
            if file {
                let path: *const AnyObject = msg_send![&*url, path];
                if !path.is_null() {
                    forward_string(&*path);
                }
            } else {
                let address: *const AnyObject = msg_send![&*url, absoluteString];
                if !address.is_null() {
                    let pointer: *const std::ffi::c_char = msg_send![&*address, UTF8String];
                    if !pointer.is_null() {
                        let link = CStr::from_ptr(pointer).to_string_lossy();
                        let _ = crate::extbus::install_plugin_link(&link);
                    }
                }
            }
        }
    }
}

fn forward_string(path: &AnyObject) {
    // SAFETY: Callers supply an NSString retained by the current AppKit event.
    let path = unsafe {
        let pointer: *const std::ffi::c_char = msg_send![path, UTF8String];
        if pointer.is_null() {
            return;
        }
        CStr::from_ptr(pointer).to_string_lossy().into_owned()
    };
    let path = std::path::PathBuf::from(path);
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("hyaplugin"))
    {
        crate::extbus::install_plugin_file(path);
    }
}

pub fn install() {
    let Some(marker) = MainThreadMarker::new() else {
        return;
    };
    let application = NSApplication::sharedApplication(marker);
    let Some(delegate) = application.delegate() else {
        return;
    };
    // SAFETY: The retained delegate is an Objective-C object with a live runtime class.
    let class = unsafe { &*objc2::ffi::object_getClass(std::ptr::from_ref(&*delegate).cast()) };
    for (selector, implementation) in [
        (
            sel!(application:openFiles:),
            open_files as extern "C-unwind" fn(_, _, _, _),
        ),
        (
            sel!(application:openURLs:),
            open_urls as extern "C-unwind" fn(_, _, _, _),
        ),
    ] {
        if class.instance_method(selector).is_some() {
            continue;
        }
        // SAFETY: Both callbacks implement void(id, SEL, id, id), matching v@:@@.
        unsafe {
            let implementation: Imp = std::mem::transmute(implementation);
            objc2::ffi::class_addMethod(
                std::ptr::from_ref(class).cast_mut(),
                selector,
                implementation,
                c"v@:@@".as_ptr(),
            );
        }
    }
}
