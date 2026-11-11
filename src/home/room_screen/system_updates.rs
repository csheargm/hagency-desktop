//! Fold only ordinary, reviewable state rows. Message-like events (including
//! undecryptable messages, calls and polls) always retain their normal rows.
use super::*;

pub(super) fn system_update_ranges(items: impl IntoIterator<Item = bool>) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    let mut end = 0;
    for (index, is_state) in items.into_iter().enumerate() {
        end = index + 1;
        if is_state { start.get_or_insert(index); }
        else if let Some(first) = start.take() { if index - first >= 3 { ranges.push(first..index); } }
    }
    if let Some(first) = start { if end - first >= 3 { ranges.push(first..end); } }
    ranges
}

pub(super) fn is_groupable_system_update(content: &TimelineItemContent) -> bool {
    match content {
        TimelineItemContent::MembershipChange(change) => !(change.change() == Some(timeline::MembershipChange::None)
            && matches!(change.content(), StateEventContentChange::Original {content, ..}
                if content.membership == MembershipState::Join || (content.membership == MembershipState::Leave && content.reason.is_none()))),
        TimelineItemContent::ProfileChange(_) => true,
        TimelineItemContent::OtherState(other) => !matches!(other.content(),
            timeline::AnyOtherStateEventContentChange::PolicyRuleRoom(_)
            | timeline::AnyOtherStateEventContentChange::PolicyRuleServer(_)
            | timeline::AnyOtherStateEventContentChange::PolicyRuleUser(_)
            | timeline::AnyOtherStateEventContentChange::RoomServerAcl(_)
            | timeline::AnyOtherStateEventContentChange::SpaceChild(_)
            | timeline::AnyOtherStateEventContentChange::SpaceParent(_)
            | timeline::AnyOtherStateEventContentChange::_Custom {..}),
        _ => false,
    }
}

pub(super) fn present_system_update_group(cx: &mut Cx, item: &WidgetRef, group: Option<(usize, bool)>) {
    item.button(cx, ids!(system_updates)).set_visible(cx, group.is_some());
    item.view(cx, ids!(body)).set_visible(cx, group.is_none_or(|(_, open)| open));
    if let Some((count, open)) = group {
        item.button(cx, ids!(system_updates)).set_text(cx, &crate::i18n::format(
            if open {"Hide {count} room updates"} else {"Show {count} room updates"}, &[("count", count.to_string())]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folding_keeps_original_indices_and_never_crosses_messages_or_virtual_rows() {
        // false: messages, errors, date/read markers, already-cleared or non-reviewable events.
        assert_eq!(system_update_ranges([false,true,true,true,true,false,true,true,false,true,true,true]), vec![1..5,9..12]);
        assert!(system_update_ranges([true,true,false,true,true]).is_empty());
        assert_eq!(system_update_ranges(std::iter::repeat_n(true,16).chain([false,false])), vec![0..16]);
    }
    #[test]
    fn collapsed_and_expanded_rows_keep_original_event_body_available() {
        let mut cx = Cx::new(Box::new(|_,_|{}));
        let item = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let v = script_eval!(vm, {mod.widgets.SmallStateEvent {}});
            let item = WidgetRef::script_from_value(vm,v);
            assert!(vm.take_errors().is_empty());
            item
        });
        item.label(&mut cx,ids!(content)).set_text(&mut cx,"Original membership event");
        present_system_update_group(&mut cx,&item,Some((16,false)));
        assert!(!item.view(&mut cx,ids!(body)).visible());
        assert!(item.button(&mut cx,ids!(system_updates)).visible());
        present_system_update_group(&mut cx,&item,Some((16,true)));
        assert!(item.view(&mut cx,ids!(body)).visible());
        assert_eq!(item.label(&mut cx,ids!(content)).text(),"Original membership event");
        present_system_update_group(&mut cx,&item,None);
        assert!(item.view(&mut cx,ids!(body)).visible());
        assert!(!item.button(&mut cx,ids!(system_updates)).visible());
    }
}

#[cfg(test)]
mod native_routing_tests {
    use super::*;
    #[test]
    fn room_lookup_reaches_timeline_beside_detached_status_and_recovers_an_empty_cache() {
        let mut cx = Cx::new(Box::new(|_,_|{}));
        let root = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);
            vm.bx.captured_errors=Some(Vec::new());
            let value=script_eval!(vm,{mod.widgets.RoomScreen {}});
            let root=WidgetRef::script_from_value(vm,value);
            assert!(vm.take_errors().is_empty());root
        });
        let mut screen=root.borrow_mut::<RoomScreen>().unwrap();
        let mut stale=screen.cached_widget_refs(&mut cx);
        assert!(!stale.portal_list.is_empty(), "detached status must not exclude the main room view from widget lookup");
        assert!(!stale.loading_pane.is_empty());
        assert!(!stale.user_profile_sliding_pane.is_empty());
        assert!(!stale.room_input_popup_menu.is_empty());
        let expected=stale.portal_list.widget_uid();
        stale.portal_list=WidgetRef::empty().as_portal_list();
        screen.cached_refs=Some(stale);
        assert_eq!(screen.cached_widget_refs(&mut cx).portal_list.widget_uid(),expected);
    }
}
