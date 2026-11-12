//! Mouse and keyboard injection via Win32.

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    mouse_event, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXSCREEN, SM_CYSCREEN,
};

pub struct MouseController {
    screen_w: i32,
    screen_h: i32,
}

impl MouseController {
    pub fn new() -> Self {
        Self {
            screen_w: unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1),
            screen_h: unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1),
        }
    }

    pub fn move_by(&self, dx: i32, dy: i32) {
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_err() {
            return;
        }
        let x = (point.x + dx).clamp(0, self.screen_w - 1);
        let y = (point.y + dy).clamp(0, self.screen_h - 1);
        unsafe {
            let _ = SetCursorPos(x, y);
        }
    }

    pub fn button(&self, button: &str, down: bool) {
        let flag = match (button == "right", down) {
            (false, true) => MOUSEEVENTF_LEFTDOWN,
            (false, false) => MOUSEEVENTF_LEFTUP,
            (true, true) => MOUSEEVENTF_RIGHTDOWN,
            (true, false) => MOUSEEVENTF_RIGHTUP,
        };
        unsafe { mouse_event(flag, 0, 0, 0, 0) };
    }

    pub fn click(&self, button: &str) {
        self.button(button, true);
        std::thread::sleep(std::time::Duration::from_millis(20));
        self.button(button, false);
    }

    pub fn wheel(&self, amount: i32) {
        unsafe { mouse_event(MOUSEEVENTF_WHEEL, 0, 0, amount, 0) };
    }
}

pub struct KeyboardController;

impl KeyboardController {
    pub fn new() -> Self {
        Self
    }

    fn key(vk: u16) {
        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn character(ch: char) {
        let code = ch as u32;
        if code > 0xFFFF {
            return;
        }
        let code = code as u16;
        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: code,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: code,
                    dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
        }
    }

    pub fn type_text(&self, text: &str) {
        for ch in text.chars() {
            match ch {
                '\n' | '\r' => Self::key(0x0D),
                '\t' => Self::key(0x09),
                '\x08' => Self::key(0x08),
                '\x1b' => Self::key(0x1B),
                _ => Self::character(ch),
            }
        }
    }
}
