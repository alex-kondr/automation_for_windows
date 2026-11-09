#![allow(non_upper_case_globals)]
//! Нативний Шар 1: Windows UI Automation через офіційний крейт `windows-rs` (`windows::Win32::UI::Accessibility`).
//! Прямі COM-виклики `IUIAutomation`, `IUIAutomationElement`, `IUIAutomationScrollPattern`, `IUIAutomationTreeWalker`
//! без проміжних Python-обгорток.

use crate::models::DetectedFile;
use std::collections::HashSet;
use windows::Win32::Foundation::POINT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationScrollPattern,
    IUIAutomationTreeWalker, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId,
    UIA_ComboBoxControlTypeId, UIA_CustomControlTypeId, UIA_DataItemControlTypeId,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_HyperlinkControlTypeId,
    UIA_ImageControlTypeId, UIA_ListControlTypeId, UIA_ListItemControlTypeId,
    UIA_MenuBarControlTypeId, UIA_MenuControlTypeId, UIA_MenuItemControlTypeId,
    UIA_PaneControlTypeId, UIA_RadioButtonControlTypeId, UIA_ScrollPatternId,
    UIA_TabControlTypeId, UIA_TabItemControlTypeId, UIA_TableControlTypeId,
    UIA_TextControlTypeId, UIA_ToolBarControlTypeId, UIA_TreeControlTypeId,
    UIA_TreeItemControlTypeId, UIA_WindowControlTypeId, UIA_CONTROLTYPE_ID,
};

#[derive(Debug, Clone, Default)]
pub struct UiaLocator {
    pub window_title: String,
    pub window_class: String,
    pub control_type: String,
    pub name: String,
    pub automation_id: String,
    pub class_name: String,
    pub bounding_rect: Option<(i32, i32, i32, i32)>,
}

/// Перетворює числовий `UIA_CONTROLTYPE_ID` у назву типу елемента (сумісно з `pywinauto`/`uiautomation`).
fn control_type_id_to_name(id: UIA_CONTROLTYPE_ID) -> &'static str {
    match id {
        UIA_ButtonControlTypeId => "Button",
        UIA_CheckBoxControlTypeId => "CheckBox",
        UIA_ComboBoxControlTypeId => "ComboBox",
        UIA_CustomControlTypeId => "Custom",
        UIA_DataItemControlTypeId => "DataItem",
        UIA_DocumentControlTypeId => "Document",
        UIA_EditControlTypeId => "Edit",
        UIA_HyperlinkControlTypeId => "Hyperlink",
        UIA_ImageControlTypeId => "Image",
        UIA_ListControlTypeId => "List",
        UIA_ListItemControlTypeId => "ListItem",
        UIA_MenuBarControlTypeId => "MenuBar",
        UIA_MenuControlTypeId => "Menu",
        UIA_MenuItemControlTypeId => "MenuItem",
        UIA_PaneControlTypeId => "Pane",
        UIA_RadioButtonControlTypeId => "RadioButton",
        UIA_TabControlTypeId => "Tab",
        UIA_TabItemControlTypeId => "TabItem",
        UIA_TableControlTypeId => "Table",
        UIA_TextControlTypeId => "Text",
        UIA_ToolBarControlTypeId => "ToolBar",
        UIA_TreeControlTypeId => "Tree",
        UIA_TreeItemControlTypeId => "TreeItem",
        UIA_WindowControlTypeId => "Window",
        _ => "Control",
    }
}

/// Створює COM-екземпляр `IUIAutomation` у поточному потоці.
fn create_uia_instance() -> Option<IUIAutomation> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
    }
}

pub struct UIAManager;

impl UIAManager {
    /// Визначає елемент інтерфейсу під вказаними координатами `(x, y)` та повертає його семантичний локатор.
    pub fn get_element_at_point(x: i32, y: i32) -> UiaLocator {
        let mut loc = UiaLocator::default();
        let uia = match create_uia_instance() {
            Some(u) => u,
            None => return loc,
        };

        unsafe {
            let elem = match uia.ElementFromPoint(POINT { x, y }) {
                Ok(e) => e,
                Err(_) => return loc,
            };

            if let Ok(bstr) = elem.CurrentName() {
                loc.name = bstr.to_string();
            }
            if let Ok(ct) = elem.CurrentControlType() {
                loc.control_type = control_type_id_to_name(ct).to_string();
            }
            if let Ok(aid) = elem.CurrentAutomationId() {
                loc.automation_id = aid.to_string();
            }
            if let Ok(cls) = elem.CurrentClassName() {
                loc.class_name = cls.to_string();
            }
            if let Ok(rect) = elem.CurrentBoundingRectangle() {
                if rect.right > rect.left && rect.bottom > rect.top {
                    loc.bounding_rect = Some((rect.left, rect.top, rect.right, rect.bottom));
                }
            }

            // Піднімаємось по дереву до вікна верхнього рівня (максимум 15 кроків)
            if let (Ok(walker), Ok(root)) = (uia.ControlViewWalker(), uia.GetRootElement()) {
                let mut curr = elem;
                for _ in 0..15 {
                    if let Ok(parent) = walker.GetParentElement(&curr) {
                        let is_root = uia
                            .CompareElements(&parent, &root)
                            .map(|b| b.as_bool())
                            .unwrap_or(false);
                        if is_root {
                            loc.window_title = curr.CurrentName().map(|s| s.to_string()).unwrap_or_default();
                            loc.window_class = curr.CurrentClassName().map(|s| s.to_string()).unwrap_or_default();
                            break;
                        }
                        curr = parent;
                    } else {
                        break;
                    }
                }
            }
        }

        loc
    }

    /// Перевіряє, чи збігається COM-елемент із цільовими параметрами локатора.
    unsafe fn is_element_matching(
        elem: &IUIAutomationElement,
        target_name_lower: &str,
        control_type_lower: &str,
        automation_id: &str,
    ) -> Option<(i32, i32)> {
        if !control_type_lower.is_empty() {
            if let Ok(ct) = elem.CurrentControlType() {
                let ct_name = control_type_id_to_name(ct).to_lowercase();
                if ct_name != control_type_lower && !control_type_lower.contains(&ct_name) {
                    return None;
                }
            }
        }

        let mut matched = false;
        if !automation_id.is_empty() {
            if let Ok(aid) = elem.CurrentAutomationId() {
                if aid.to_string() == automation_id {
                    matched = true;
                }
            }
        }

        if !matched && !target_name_lower.is_empty() {
            if let Ok(name_bstr) = elem.CurrentName() {
                let ctrl_name = name_bstr.to_string().trim().to_lowercase();
                if !ctrl_name.is_empty()
                    && (ctrl_name == target_name_lower || ctrl_name.contains(target_name_lower))
                {
                    matched = true;
                }
            }
        }

        if matched {
            if let Ok(rect) = elem.CurrentBoundingRectangle() {
                if rect.right > rect.left && rect.bottom > rect.top {
                    return Some(((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2));
                }
            }
        }
        None
    }

    /// Рекурсивний обхід піддерева UIA з обмеженням глибини.
    unsafe fn walk_find(
        walker: &IUIAutomationTreeWalker,
        node: &IUIAutomationElement,
        target_name_lower: &str,
        control_type_lower: &str,
        automation_id: &str,
        depth: u32,
        max_depth: u32,
    ) -> Option<(i32, i32)> {
        if let Some(coords) =
            Self::is_element_matching(node, target_name_lower, control_type_lower, automation_id)
        {
            return Some(coords);
        }
        if depth >= max_depth {
            return None;
        }

        let mut child_opt = walker.GetFirstChildElement(node).ok();
        let mut visited_siblings = 0;
        while let Some(child) = child_opt {
            if visited_siblings > 250 {
                break;
            }
            visited_siblings += 1;
            if let Some(found) = Self::walk_find(
                walker,
                &child,
                target_name_lower,
                control_type_lower,
                automation_id,
                depth + 1,
                max_depth,
            ) {
                return Some(found);
            }
            child_opt = walker.GetNextSiblingElement(&child).ok();
        }
        None
    }

    /// Шукає елемент за його UIA-локатором та повертає екранні координати його центру `(center_x, center_y)`.
    pub fn find_element(
        name: &str,
        control_type: &str,
        window_title: &str,
        automation_id: &str,
        expected_x: Option<i32>,
        expected_y: Option<i32>,
    ) -> Option<(i32, i32)> {
        let name_trimmed = name.trim();
        let aid_trimmed = automation_id.trim();
        if name_trimmed.is_empty() && aid_trimmed.is_empty() {
            return None;
        }

        let name_lower = name_trimmed.to_lowercase();
        let ct_lower = control_type.trim().replace("Control", "").to_lowercase();
        let win_lower = window_title.trim().to_lowercase();

        let uia = create_uia_instance()?;

        unsafe {
            // 1. Надшвидкий шлях (<0.3 мс): перевіряємо елемент безпосередньо в очікуваній точці
            if let (Some(ex), Some(ey)) = (expected_x, expected_y) {
                if ex > 0 || ey > 0 {
                    if let Ok(pt_elem) = uia.ElementFromPoint(POINT { x: ex, y: ey }) {
                        if let Some(coords) = Self::is_element_matching(
                            &pt_elem,
                            &name_lower,
                            &ct_lower,
                            aid_trimmed,
                        ) {
                            return Some(coords);
                        }
                    }
                }
            }

            // 2. Пошук у цільовому або активному вікні
            let walker = uia.ControlViewWalker().ok()?;
            let root = uia.GetRootElement().ok()?;

            let mut search_scope: Option<IUIAutomationElement> = None;
            if !win_lower.is_empty() {
                let mut child_opt = walker.GetFirstChildElement(&root).ok();
                while let Some(w) = child_opt {
                    if let Ok(wname) = w.CurrentName() {
                        if wname.to_string().to_lowercase().contains(&win_lower) {
                            search_scope = Some(w);
                            break;
                        }
                    }
                    child_opt = walker.GetNextSiblingElement(&w).ok();
                }
            }

            if search_scope.is_none() {
                let fg_hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
                if !fg_hwnd.is_invalid() {
                    search_scope = uia.ElementFromHandle(fg_hwnd).ok();
                }
            }

            let scope = search_scope.unwrap_or(root);
            Self::walk_find(&walker, &scope, &name_lower, &ct_lower, aid_trimmed, 0, 7)
        }
    }

    /// Зчитує реальні елементи списків та таблиць через UI Automation (`ListItem`, `DataItem`, `TreeItem`, `Text`).
    pub fn get_list_items(
        window_title: &str,
        area_rect: Option<(i32, i32, i32, i32)>,
    ) -> Vec<DetectedFile> {
        let mut results = Vec::new();
        let uia = match create_uia_instance() {
            Some(u) => u,
            None => return results,
        };

        unsafe {
            let walker = match uia.ControlViewWalker() {
                Ok(w) => w,
                Err(_) => return results,
            };
            let root = match uia.GetRootElement() {
                Ok(r) => r,
                Err(_) => return results,
            };

            let win_lower = window_title.trim().to_lowercase();
            let mut search_scope: Option<IUIAutomationElement> = None;
            if !win_lower.is_empty() {
                let mut child_opt = walker.GetFirstChildElement(&root).ok();
                while let Some(w) = child_opt {
                    if let Ok(wname) = w.CurrentName() {
                        if wname.to_string().to_lowercase().contains(&win_lower) {
                            search_scope = Some(w);
                            break;
                        }
                    }
                    child_opt = walker.GetNextSiblingElement(&w).ok();
                }
            }

            if search_scope.is_none() {
                let fg_hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
                if !fg_hwnd.is_invalid() {
                    search_scope = uia.ElementFromHandle(fg_hwnd).ok();
                }
            }

            let scope = search_scope.unwrap_or(root);

            unsafe fn collect_items(
                walker: &IUIAutomationTreeWalker,
                node: &IUIAutomationElement,
                area_rect: Option<(i32, i32, i32, i32)>,
                depth: u32,
                out: &mut Vec<DetectedFile>,
            ) {
                if let Ok(ct) = node.CurrentControlType() {
                    let is_item = matches!(
                        ct,
                        UIA_ListItemControlTypeId
                            | UIA_DataItemControlTypeId
                            | UIA_TreeItemControlTypeId
                            | UIA_CustomControlTypeId
                            | UIA_TextControlTypeId
                    );
                    if is_item {
                        if let (Ok(name_bstr), Ok(rect)) =
                            (node.CurrentName(), node.CurrentBoundingRectangle())
                        {
                            let name = name_bstr.to_string().trim().to_string();
                            if !name.is_empty() && rect.right > rect.left && rect.bottom > rect.top {
                                let cx = (rect.left + rect.right) / 2;
                                let cy = (rect.top + rect.bottom) / 2;
                                let inside = match area_rect {
                                    Some((x1, y1, x2, y2)) => {
                                        cx >= x1 && cx <= x2 && cy >= y1 && cy <= y2
                                    }
                                    None => true,
                                };
                                if inside {
                                    out.push(DetectedFile {
                                        name,
                                        x: cx,
                                        y: cy,
                                        control_type: Some(control_type_id_to_name(ct).to_string()),
                                    });
                                }
                            }
                        }
                    }
                }

                if depth >= 9 || out.len() >= 200 {
                    return;
                }

                let mut child_opt = walker.GetFirstChildElement(node).ok();
                let mut count = 0;
                while let Some(child) = child_opt {
                    if count > 250 {
                        break;
                    }
                    count += 1;
                    collect_items(walker, &child, area_rect, depth + 1, out);
                    child_opt = walker.GetNextSiblingElement(&child).ok();
                }
            }

            collect_items(&walker, &scope, area_rect, 0, &mut results);
        }

        // Усунення дублікатів за назвою та орієнтовними координатами
        let mut unique = Vec::with_capacity(results.len());
        let mut seen = HashSet::new();
        for item in results {
            let key = (item.name.clone(), item.x / 10, item.y / 10);
            if seen.insert(key) {
                unique.push(item);
            }
        }
        unique
    }

    /// Прокручує список або таблицю до самого кінця виключно по вертикалі (`SetScrollPercent(-1.0, 100.0)`).
    pub fn scroll_to_bottom_uia(x: Option<i32>, y: Option<i32>) -> bool {
        let (px, py) = match (x, y) {
            (Some(px), Some(py)) => (px, py),
            _ => return false,
        };

        let uia = match create_uia_instance() {
            Some(u) => u,
            None => return false,
        };

        unsafe {
            let mut curr = match uia.ElementFromPoint(POINT { x: px, y: py }) {
                Ok(e) => e,
                Err(_) => return false,
            };
            let walker = uia.ControlViewWalker().ok();

            for _ in 0..6 {
                if let Ok(pat_unk) = curr.GetCurrentPattern(UIA_ScrollPatternId) {
                    if let Ok(scroll_pat) = windows::core::Interface::cast::<IUIAutomationScrollPattern>(&pat_unk) {
                        // -1.0 (UIA_ScrollPatternNoScroll) гарантує відсутність горизонтального зсуву
                        if scroll_pat.SetScrollPercent(-1.0, 100.0).is_ok() {
                            return true;
                        }
                    }
                }
                if let Some(ref w) = walker {
                    if let Ok(parent) = w.GetParentElement(&curr) {
                        curr = parent;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        false
    }
}
