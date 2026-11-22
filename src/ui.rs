//! Slint window. This module only copies published snapshots onto the UI thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::config::{history_capacity, HISTORY_WINDOW, UI_REFRESH_INTERVAL};
use crate::format::DisplayKind;
use crate::launch::LaunchOptions;
use crate::model::{ResourceId, Snapshot};
use crate::pick::{
    axis_of, displayed, format_age, index_to_fraction, select_slot, slot_of_id, AxisSample,
    SelectionState,
};
use crate::present::{present_cards, present_detail_at, present_rows, status_line, PresentedRow};
use crate::rank::{empty_rank_text, rank, rank_caption, RankStatus};
use crate::sampler::{PublishedSample, SamplerHandle};
use crate::series::{graph_points, scale_peak, scale_percent, ChartHistory};
use crate::sort::SortColumn;

slint::include_modules!();

struct RankCache {
    id: u64,
    resource: ResourceId,
    adapter: Option<u64>,
    rows: Vec<TopRow>,
    caption: String,
    empty: String,
}

struct UiState {
    sort_column: SortColumn,
    sort_desc: bool,
    resource: ResourceId,
    last_sequence: Option<u64>,
    selection: SelectionState,
    timeline: Vec<Option<Arc<Snapshot>>>,
    adapter: Option<u64>,
    rank_cache: Option<RankCache>,
    pending_slot: Option<i32>,
    rows: Rc<VecModel<ProcessRow>>,
    top: Rc<VecModel<TopRow>>,
    detail_points: Rc<VecModel<f32>>,
    cpu_spark: Rc<VecModel<f32>>,
    memory_spark: Rc<VecModel<f32>>,
    disk_spark: Rc<VecModel<f32>>,
    network_spark: Rc<VecModel<f32>>,
    gpu3d_spark: Rc<VecModel<f32>>,
    gpu_encode_spark: Rc<VecModel<f32>>,
    gpu_decode_spark: Rc<VecModel<f32>>,
    vram_spark: Rc<VecModel<f32>>,
    shared_spark: Rc<VecModel<f32>>,
}

pub fn run(options: LaunchOptions) -> Result<(), String> {
    let sampler = SamplerHandle::start(options.demo.as_deref())?;
    let ui = AppWindow::new().map_err(|err| format!("ウィンドウを作成できませんでした: {err}"))?;
    ui.set_version_text(format!("PULSE {}", env!("CARGO_PKG_VERSION")).into());
    ui.set_page(options.page);
    if let Some(resource) = options.resource {
        ui.set_resource(resource);
    }
    ui.set_demo(options.demo.is_some());
    if let Some(theme) = options.theme {
        ui.global::<Theme>().set_preference(theme);
    }
    if let Some((width, height)) = options.size {
        ui.window().set_size(slint::LogicalSize::new(width, height));
    }
    // System mode follows AppsUseLightTheme. Missing key stays light.
    ui.global::<Theme>()
        .set_system_light(crate::platform::apps_use_light_theme().unwrap_or(true));

    let state = Rc::new(RefCell::new(UiState {
        sort_column: SortColumn::Cpu,
        sort_desc: true,
        resource: ResourceId::from_index(options.resource.unwrap_or(0)),
        last_sequence: None,
        selection: SelectionState::live(),
        timeline: Vec::new(),
        adapter: None,
        rank_cache: None,
        pending_slot: options.select_history,
        rows: Rc::new(VecModel::from(Vec::<ProcessRow>::new())),
        top: Rc::new(VecModel::from(Vec::<TopRow>::new())),
        detail_points: Rc::new(VecModel::from(Vec::<f32>::new())),
        cpu_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        memory_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        disk_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        network_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        gpu3d_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        gpu_encode_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        gpu_decode_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        vram_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
        shared_spark: Rc::new(VecModel::from(Vec::<f32>::new())),
    }));
    {
        let state = state.borrow();
        ui.set_rows(ModelRc::from(state.rows.clone()));
        ui.set_top_rows(ModelRc::from(state.top.clone()));
        ui.set_detail_points(ModelRc::from(state.detail_points.clone()));
        ui.set_cpu_spark(ModelRc::from(state.cpu_spark.clone()));
        ui.set_memory_spark(ModelRc::from(state.memory_spark.clone()));
        ui.set_disk_spark(ModelRc::from(state.disk_spark.clone()));
        ui.set_network_spark(ModelRc::from(state.network_spark.clone()));
        ui.set_gpu3d_spark(ModelRc::from(state.gpu3d_spark.clone()));
        ui.set_gpu_encode_spark(ModelRc::from(state.gpu_encode_spark.clone()));
        ui.set_gpu_decode_spark(ModelRc::from(state.gpu_decode_spark.clone()));
        ui.set_vram_spark(ModelRc::from(state.vram_spark.clone()));
        ui.set_shared_spark(ModelRc::from(state.shared_spark.clone()));
    }

    let latest = sampler.latest();
    wire_sort(&ui, &state, latest.clone());
    wire_resource(&ui, &state, latest.clone());
    wire_history(&ui, &state, latest.clone());
    let prefs = Rc::new(RefCell::new(session_prefs(&options)?));
    apply_prefs(&ui, &prefs.borrow());
    if !prefs.borrow().resource_shown[state.borrow().resource as usize] {
        let next = prefs
            .borrow()
            .resource_order
            .iter()
            .copied()
            .find(|id| prefs.borrow().resource_shown[*id as usize])
            .unwrap_or(0);
        state.borrow_mut().resource = ResourceId::from_index(next);
        ui.set_resource(next);
    }
    wire_layout(&ui, &state, latest.clone(), &prefs);
    if options.menu.as_deref() == Some("columns") {
        ui.set_page(0);
        ui.set_open_column_menu(true);
    } else if options.menu.as_deref() == Some("resources") {
        ui.set_page(1);
        ui.set_open_resource_menu(true);
    }

    if let Some(sample) = latest.load() {
        apply(&ui, &state, &sample, true);
    }

    let timer = Timer::default();
    let weak = ui.as_weak();
    let timer_state = Rc::clone(&state);
    let timer_latest = latest;
    let theme_ticks = Cell::new(0u32);
    timer.start(TimerMode::Repeated, UI_REFRESH_INTERVAL, move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if let Some(sample) = timer_latest.load() {
            apply(&ui, &timer_state, &sample, false);
        }
        // Re-read the Windows app theme about every two seconds, and only while
        // the choice is "system". A manual light/dark choice is left alone.
        theme_ticks.set(theme_ticks.get().wrapping_add(1));
        if theme_ticks.get().is_multiple_of(8) {
            let theme = ui.global::<Theme>();
            if theme.get_preference() == 2 {
                if let Some(light) = crate::platform::apps_use_light_theme() {
                    if theme.get_system_light() != light {
                        theme.set_system_light(light);
                    }
                }
            }
        }
    });

    ui.run()
        .map_err(|err| format!("UI の実行に失敗しました: {err}"))?;
    // Keep the timer alive until the window closes, then stop the sampler.
    drop(timer);
    drop(sampler);
    Ok(())
}

fn wire_sort(ui: &AppWindow, state: &Rc<RefCell<UiState>>, latest: crate::sampler::SharedLatest) {
    let ui_weak = ui.as_weak();
    let state = Rc::clone(state);
    ui.on_sort_requested(move |column| {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        {
            let mut state = state.borrow_mut();
            let requested = SortColumn::parse(column.as_str());
            if state.sort_column == requested {
                state.sort_desc = !state.sort_desc;
            } else {
                state.sort_column = requested;
                state.sort_desc = requested.default_desc();
            }
            ui.set_sort_column(requested.as_str().into());
            ui.set_sort_desc(state.sort_desc);
        }
        if let Some(sample) = latest.load() {
            apply(&ui, &state, &sample, true);
        }
    });
}

fn session_prefs(options: &LaunchOptions) -> Result<crate::uiprefs::UiPrefs, String> {
    let mut prefs = crate::uiprefs::load();
    if let Some(name) = &options.hide_column {
        if !prefs.hide_column_named(name) {
            return Err(format!(
                "隠せない列です: {name}（name は常に表示。pid / cpu / memory / disk / network / gpu3d / gpu-encode / gpu-decode / vram / shared）"
            ));
        }
    }
    if let Some(name) = &options.hide_resource {
        if crate::uiprefs::resource_id(name).is_none() || !prefs.hide_resource_named(name) {
            return Err(format!(
                "隠せない資源です: {name}（cpu / memory / disk / network / gpu3d / gpu-encode / gpu-decode / vram / shared）"
            ));
        }
    }
    Ok(prefs)
}

fn apply_prefs(ui: &AppWindow, prefs: &crate::uiprefs::UiPrefs) {
    let columns = ui.global::<Columns>();
    columns.set_order(ModelRc::new(VecModel::from(prefs.column_order.clone())));
    columns.set_shown(ModelRc::new(VecModel::from(prefs.column_shown.clone())));
    ui.set_resource_order(ModelRc::new(VecModel::from(prefs.resource_order.clone())));
    ui.set_resource_shown(ModelRc::new(VecModel::from(prefs.resource_shown.clone())));
    ui.set_resource_pref(prefs.resource_width);
}

fn column_widths(ui: &AppWindow) -> Vec<f32> {
    let columns = ui.global::<Columns>();
    vec![
        columns.get_name(),
        columns.get_pid(),
        columns.get_cpu(),
        columns.get_memory(),
        columns.get_disk(),
        columns.get_network(),
        columns.get_gpu3d(),
        columns.get_gpu_encode(),
        columns.get_gpu_decode(),
        columns.get_vram(),
        columns.get_shared(),
    ]
}

fn wire_layout(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    latest: crate::sampler::SharedLatest,
    prefs: &Rc<RefCell<crate::uiprefs::UiPrefs>>,
) {
    {
        let ui_weak = ui.as_weak();
        let prefs = Rc::clone(prefs);
        ui.on_column_toggled(move |id| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            prefs.borrow_mut().toggle_column(id);
            apply_prefs(&ui, &prefs.borrow());
            crate::uiprefs::save(&prefs.borrow());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let prefs = Rc::clone(prefs);
        ui.on_column_moved(move |id, delta| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            prefs.borrow_mut().move_column(id, delta);
            apply_prefs(&ui, &prefs.borrow());
            crate::uiprefs::save(&prefs.borrow());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let prefs = Rc::clone(prefs);
        ui.on_column_dragged(move |id, dx| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let widths = column_widths(&ui);
            prefs.borrow_mut().drag_column(id, dx, &widths);
            apply_prefs(&ui, &prefs.borrow());
            crate::uiprefs::save(&prefs.borrow());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let prefs = Rc::clone(prefs);
        let state = Rc::clone(state);
        let latest = latest.clone();
        ui.on_resource_toggled(move |id| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let selected = ui.get_resource();
            let next = prefs.borrow_mut().toggle_resource(id, selected);
            apply_prefs(&ui, &prefs.borrow());
            crate::uiprefs::save(&prefs.borrow());
            if next != selected {
                ui.set_resource(next);
                state.borrow_mut().resource = ResourceId::from_index(next);
                if let Some(sample) = latest.load() {
                    apply(&ui, &state, &sample, true);
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let prefs = Rc::clone(prefs);
        ui.on_resource_moved(move |id, delta| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            prefs.borrow_mut().move_resource(id, delta);
            apply_prefs(&ui, &prefs.borrow());
            crate::uiprefs::save(&prefs.borrow());
        });
    }
    {
        let prefs = Rc::clone(prefs);
        ui.on_resource_resized(move |width| {
            prefs.borrow_mut().resource_width = width;
            crate::uiprefs::save(&prefs.borrow());
        });
    }
}

fn wire_history(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    latest: crate::sampler::SharedLatest,
) {
    let ui_weak = ui.as_weak();
    let picked_state = Rc::clone(state);
    let picked_latest = latest.clone();
    ui.on_history_picked(move |index| {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        pin_slot(&mut picked_state.borrow_mut(), index);
        if let Some(sample) = picked_latest.load() {
            apply(&ui, &picked_state, &sample, true);
        }
    });

    let ui_weak = ui.as_weak();
    let hover_state = Rc::clone(state);
    ui.on_history_hovered(move |index| {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        let preview = hover_text(&hover_state.borrow(), index);
        ui.set_history_hover(preview.into());
    });

    let ui_weak = ui.as_weak();
    let live_state = Rc::clone(state);
    let live_latest = latest.clone();
    ui.on_history_live(move || {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        let mut state = live_state.borrow_mut();
        state.selection.clear();
        state.rank_cache = None;
        drop(state);
        if let Some(sample) = live_latest.load() {
            apply(&ui, &live_state, &sample, true);
        }
    });

    let ui_weak = ui.as_weak();
    let gpu_state = Rc::clone(state);
    let gpu_latest = latest;
    ui.on_gpu_picked(move |index| {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        {
            let mut state = gpu_state.borrow_mut();
            let luid = state.timeline.iter().rev().flatten().find_map(|snapshot| {
                let mut gpus = snapshot.gpus.clone();
                gpus.sort_by_key(|gpu| gpu.luid);
                gpus.get(index as usize).map(|gpu| gpu.luid)
            });
            if let Some(luid) = luid {
                state.adapter = Some(luid);
                state.rank_cache = None;
            }
        }
        if let Some(sample) = gpu_latest.load() {
            apply(&ui, &gpu_state, &sample, true);
        }
    });
}

fn wire_resource(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    latest: crate::sampler::SharedLatest,
) {
    let ui_weak = ui.as_weak();
    let state = Rc::clone(state);
    ui.on_resource_requested(move |index| {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        state.borrow_mut().resource = ResourceId::from_index(index);
        if let Some(sample) = latest.load() {
            apply(&ui, &state, &sample, true);
        }
    });
}

fn apply(ui: &AppWindow, state: &Rc<RefCell<UiState>>, sample: &PublishedSample, force: bool) {
    let (column, descending, resource, rows_model) = {
        let mut state = state.borrow_mut();
        if !force && state.last_sequence == Some(sample.snapshot.sequence) {
            return;
        }
        state.last_sequence = Some(sample.snapshot.sequence);
        state.timeline = sample.timeline.clone();
        if let Some(index) = state.pending_slot.take() {
            pin_slot(&mut state, index);
        }
        if gpu_resource(state.resource) && state.adapter.is_none() {
            state.adapter = sample.snapshot.gpus.iter().map(|gpu| gpu.luid).min();
        }
        (
            state.sort_column,
            state.sort_desc,
            state.resource,
            Rc::clone(&state.rows),
        )
    };

    let presented = present_rows(&sample.snapshot, column, descending);
    sync_model(&rows_model, presented.into_iter().map(to_row).collect());
    paint_graphs(ui, state, resource, &sample.charts);
    paint_history(ui, state, resource, sample);

    let line = status_line(
        &sample.snapshot,
        sample.self_cpu,
        sample.self_memory,
        &sample.updated_text,
    );
    let line = if sample.demo {
        format!("DEMO DATA    {line}")
    } else {
        line
    };
    ui.set_demo(sample.demo);
    ui.set_status_text(line.into());
    match &sample.snapshot.warning {
        Some(warning) => {
            ui.set_warning_text(warning.into());
            ui.set_warning_visible(true);
        }
        None => {
            ui.set_warning_text("".into());
            ui.set_warning_visible(false);
        }
    }

    let cards = present_cards(&sample.snapshot);
    ui.set_cpu_summary(cards.cpu.text.into());
    ui.set_cpu_kind(cards.cpu.kind.code());
    ui.set_memory_summary(cards.memory.text.into());
    ui.set_memory_kind(cards.memory.kind.code());
    ui.set_disk_summary(cards.disk.text.into());
    ui.set_disk_kind(cards.disk.kind.code());
    ui.set_network_summary(cards.network.text.into());
    ui.set_network_kind(cards.network.kind.code());
    ui.set_gpu3d_summary(cards.gpu3d.text.into());
    ui.set_gpu3d_kind(cards.gpu3d.kind.code());
    ui.set_gpu_encode_summary(cards.gpu_encode.text.into());
    ui.set_gpu_encode_kind(cards.gpu_encode.kind.code());
    ui.set_gpu_decode_summary(cards.gpu_decode.text.into());
    ui.set_gpu_decode_kind(cards.gpu_decode.kind.code());
    ui.set_vram_summary(cards.vram_dedicated.text.into());
    ui.set_vram_kind(cards.vram_dedicated.kind.code());
    ui.set_shared_summary(cards.vram_shared.text.into());
    ui.set_shared_kind(cards.vram_shared.kind.code());
}

fn paint_history(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    resource: ResourceId,
    sample: &PublishedSample,
) {
    let (adapter, shown, cursor, pinned, label, rows, caption, empty) = {
        let mut state = state.borrow_mut();
        let adapter = if gpu_resource(resource) {
            state.adapter
        } else {
            None
        };
        let shown = state
            .selection
            .selected()
            .cloned()
            .unwrap_or_else(|| Arc::clone(&sample.snapshot));
        let axis = axis_samples(&state.timeline);
        let cursor = state
            .selection
            .selected()
            .and_then(|selected| slot_of_id(&axis, selected.sequence));
        let pinned = !state.selection.is_live();
        let label = match state.selection.selected() {
            Some(selected) => history_label(selected, &sample.snapshot, cursor.is_some()),
            None => String::new(),
        };
        let key = state
            .selection
            .selected()
            .map(|selected| selected.sequence)
            .unwrap_or(sample.snapshot.sequence);
        let cache_hit = state.rank_cache.as_ref().is_some_and(|cache| {
            cache.id == key && cache.resource == resource && cache.adapter == adapter
        });
        if !cache_hit {
            let view = displayed(
                sample.snapshot.as_ref(),
                state.selection.selected().map(|snapshot| snapshot.as_ref()),
            );
            let status = rank(view, resource, adapter, 5);
            let rows = match &status {
                RankStatus::Ranked(ranked) => ranked
                    .iter()
                    .map(|row| TopRow {
                        rank: row.rank.to_string().into(),
                        name: row.name.as_str().into(),
                        pid: row.pid.to_string().into(),
                        value: row.value.as_str().into(),
                        kind: row.kind.code(),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            state.rank_cache = Some(RankCache {
                id: key,
                resource,
                adapter,
                rows,
                caption: rank_caption(resource, adapter.is_some()).to_string(),
                empty: empty_rank_text(&status).unwrap_or("").to_string(),
            });
        }
        let cache = state.rank_cache.as_ref().expect("rank cache");
        (
            adapter,
            shown,
            cursor,
            pinned,
            label,
            cache.rows.clone(),
            cache.caption.clone(),
            cache.empty.clone(),
        )
    };

    let detail = present_detail_at(shown.as_ref(), resource, adapter);
    ui.set_detail_title(detail.title.into());
    ui.set_detail_value(detail.value.into());
    ui.set_detail_kind(detail.kind.code());
    ui.set_detail_extra(detail.extra.into());
    ui.set_detail_note(detail.note.into());
    ui.set_detail_graph(detail.show_graph);
    ui.set_history_cursor(cursor.map(|index| index as i32).unwrap_or(-1));
    ui.set_history_pinned(pinned);
    ui.set_history_label(label.into());
    ui.set_rank_caption(caption.into());
    ui.set_rank_empty(
        if empty.is_empty() {
            "実測値のあるプロセスはありません。"
        } else {
            empty.as_str()
        }
        .into(),
    );
    sync_model(&state.borrow().top, rows);

    let points = {
        let state = state.borrow();
        graph_points(
            state.timeline.iter().map(|slot| slot.as_deref()),
            history_capacity(),
            resource,
            adapter,
        )
    };
    let scaled = scale_for(resource, &points);
    ui.set_graph_axis_high(scaled.axis_high.into());
    ui.set_graph_axis_low(scaled.axis_low.into());
    sync_model(&state.borrow().detail_points, scaled.points);
    publish_gpu_choices(ui, sample, adapter);
}

fn publish_gpu_choices(ui: &AppWindow, sample: &PublishedSample, adapter: Option<u64>) {
    let mut gpus = sample.snapshot.gpus.clone();
    gpus.sort_by_key(|gpu| gpu.luid);
    let names: Vec<slint::SharedString> = if adapter.is_some() {
        gpus.iter().map(|gpu| gpu.name.as_str().into()).collect()
    } else {
        Vec::new()
    };
    let choice = adapter
        .and_then(|luid| gpus.iter().position(|gpu| gpu.luid == luid))
        .map(|index| index as i32)
        .unwrap_or(-1);
    ui.set_gpu_names(ModelRc::new(VecModel::from(names)));
    ui.set_gpu_choice(choice);
}

fn pin_slot(state: &mut UiState, index: i32) {
    if index < 0 {
        return;
    }
    let axis = axis_samples(&state.timeline);
    let Some(len) = usize::try_from(index).ok() else {
        return;
    };
    let fraction = index_to_fraction(axis.len(), len);
    let window_ns = u64::try_from(HISTORY_WINDOW.as_nanos()).unwrap_or(60_000_000_000);
    let Some(slot) = select_slot(&axis, fraction, window_ns) else {
        return;
    };
    let Some(snapshot) = state.timeline.get(slot).and_then(|slot| slot.clone()) else {
        return;
    };
    state.selection.pin(snapshot);
    state.rank_cache = None;
}

fn hover_text(state: &UiState, index: i32) -> String {
    if index < 0 {
        return String::new();
    }
    let Some(slot) = usize::try_from(index)
        .ok()
        .and_then(|index| state.timeline.get(index))
        .and_then(|slot| slot.as_ref())
    else {
        return String::new();
    };
    let adapter = if gpu_resource(state.resource) {
        state.adapter
    } else {
        None
    };
    let detail = present_detail_at(slot, state.resource, adapter);
    let clock = crate::platform::format_local_hms(slot.taken_at);
    format!("{clock}  {}", detail.value)
}

fn history_label(selected: &Snapshot, latest: &Snapshot, in_window: bool) -> String {
    let age = format_age(
        latest
            .monotonic
            .saturating_duration_since(selected.monotonic),
    );
    let clock = crate::platform::format_local_hms(selected.taken_at);
    let interval = match selected.interval {
        Some(interval) => format!("区間 {} ms", interval.as_millis()),
        None => "区間は未確定".to_string(),
    };
    if in_window {
        format!("選択中 {clock}（{age}）  {interval}")
    } else {
        format!("選択中：{age}（{clock}、{interval}）。この時点は直近 60 秒のグラフの外です")
    }
}

fn axis_samples(timeline: &[Option<Arc<Snapshot>>]) -> Vec<Option<AxisSample>> {
    let Some(newest) = timeline
        .iter()
        .flatten()
        .map(|snapshot| snapshot.monotonic)
        .max()
    else {
        return vec![None; timeline.len()];
    };
    let origin = newest.checked_sub(HISTORY_WINDOW).unwrap_or(newest);
    axis_of(timeline, |snapshot| {
        u64::try_from(
            snapshot
                .monotonic
                .saturating_duration_since(origin)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX)
    })
}

fn gpu_resource(resource: ResourceId) -> bool {
    matches!(
        resource,
        ResourceId::Gpu3d
            | ResourceId::GpuEncode
            | ResourceId::GpuDecode
            | ResourceId::VramDedicated
            | ResourceId::VramShared
    )
}

fn scale_for(resource: ResourceId, points: &[Option<f64>]) -> crate::series::ScaledSeries {
    match resource {
        ResourceId::Cpu
        | ResourceId::Memory
        | ResourceId::Gpu3d
        | ResourceId::GpuEncode
        | ResourceId::GpuDecode => scale_percent(points),
        ResourceId::Disk | ResourceId::Network => scale_peak(points, crate::series::rate_label),
        ResourceId::VramDedicated | ResourceId::VramShared => {
            scale_peak(points, crate::series::bytes_label)
        }
    }
}

fn paint_graphs(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    resource: ResourceId,
    charts: &ChartHistory,
) {
    let borrowed = state.borrow();
    sync_model(
        &borrowed.cpu_spark,
        spark(&scale_percent(&charts.cpu).points),
    );
    sync_model(
        &borrowed.memory_spark,
        spark(&scale_percent(&charts.memory).points),
    );
    sync_model(
        &borrowed.disk_spark,
        spark(&scale_peak(&charts.disk, crate::series::rate_label).points),
    );
    sync_model(
        &borrowed.network_spark,
        spark(&scale_peak(&charts.network, crate::series::rate_label).points),
    );
    sync_model(
        &borrowed.gpu3d_spark,
        spark(&scale_percent(&charts.gpu_3d).points),
    );
    sync_model(
        &borrowed.gpu_encode_spark,
        spark(&scale_percent(&charts.gpu_encode).points),
    );
    sync_model(
        &borrowed.gpu_decode_spark,
        spark(&scale_percent(&charts.gpu_decode).points),
    );
    sync_model(
        &borrowed.vram_spark,
        spark(&scale_peak(&charts.vram, crate::series::bytes_label).points),
    );
    sync_model(
        &borrowed.shared_spark,
        spark(&scale_peak(&charts.shared, crate::series::bytes_label).points),
    );
    let (scaled, _) = scale_resource(resource, charts);
    ui.set_graph_axis_high(scaled.axis_high.into());
    ui.set_graph_axis_low(scaled.axis_low.into());
    sync_model(&borrowed.detail_points, scaled.points);
}

fn scale_resource(
    resource: ResourceId,
    charts: &ChartHistory,
) -> (crate::series::ScaledSeries, ()) {
    let scaled = match resource {
        ResourceId::Cpu => scale_percent(&charts.cpu),
        ResourceId::Memory => scale_percent(&charts.memory),
        ResourceId::Disk => scale_peak(&charts.disk, crate::series::rate_label),
        ResourceId::Network => scale_peak(&charts.network, crate::series::rate_label),
        ResourceId::Gpu3d => scale_percent(&charts.gpu_3d),
        ResourceId::GpuEncode => scale_percent(&charts.gpu_encode),
        ResourceId::GpuDecode => scale_percent(&charts.gpu_decode),
        ResourceId::VramDedicated => scale_peak(&charts.vram, crate::series::bytes_label),
        ResourceId::VramShared => scale_peak(&charts.shared, crate::series::bytes_label),
    };
    (scaled, ())
}

fn spark(points: &[f32]) -> Vec<f32> {
    crate::series::downsample(points, 36)
}

fn to_row(row: PresentedRow) -> ProcessRow {
    ProcessRow {
        name: row.name.into(),
        pid: row.pid.into(),
        cpu: row.cpu.into(),
        cpu_kind: to_kind(row.cpu_kind),
        memory: row.memory.into(),
        memory_kind: to_kind(row.memory_kind),
        disk: row.disk.into(),
        disk_kind: to_kind(row.disk_kind),
        network: row.network.into(),
        network_kind: to_kind(row.network_kind),
        gpu3d: row.gpu3d.into(),
        gpu3d_kind: to_kind(row.gpu3d_kind),
        gpu_encode: row.gpu_encode.into(),
        gpu_encode_kind: to_kind(row.gpu_encode_kind),
        gpu_decode: row.gpu_decode.into(),
        gpu_decode_kind: to_kind(row.gpu_decode_kind),
        vram: row.vram.into(),
        vram_kind: to_kind(row.vram_kind),
        shared: row.shared.into(),
        shared_kind: to_kind(row.shared_kind),
    }
}

fn to_kind(kind: DisplayKind) -> CellKind {
    match kind {
        DisplayKind::Value => CellKind::Value,
        DisplayKind::Zero => CellKind::Zero,
        DisplayKind::Unavailable => CellKind::Unavailable,
        DisplayKind::Unsupported => CellKind::Unsupported,
    }
}

fn sync_model<T: Clone + 'static>(model: &VecModel<T>, mut next: Vec<T>) {
    let target = next.len();
    let shared = model.row_count().min(target);
    for (index, item) in next.drain(..shared).enumerate() {
        model.set_row_data(index, item);
    }
    for item in next {
        model.push(item);
    }
    while model.row_count() > target {
        let _ = model.remove(model.row_count() - 1);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use i_slint_backend_testing::ElementHandle;
    use slint::{ComponentHandle, ModelRc, VecModel};
    use std::cell::Cell;
    use std::rc::Rc;

    fn backend() {
        // `init_no_event_loop` installs a per-thread backend. A process-wide
        // Once leaves other test threads on Winit, which CI cannot open
        // because it has no DISPLAY.
        thread_local! {
            static READY: Cell<bool> = const { Cell::new(false) };
        }
        READY.with(|ready| {
            if !ready.get() {
                i_slint_backend_testing::init_no_event_loop();
                ready.set(true);
            }
        });
    }

    fn element(ui: &AppWindow, name: &str) -> ElementHandle {
        let qualified = format!("AppWindow::{name}");
        let found = ElementHandle::find_by_element_id(ui, qualified.as_str()).next();
        found.unwrap_or_else(|| panic!("missing element {name}"))
    }

    fn sample_row(index: i32) -> ProcessRow {
        let text: slint::SharedString = "1.0%".into();
        let bytes: slint::SharedString = "1.0 MB".into();
        ProcessRow {
            name: format!("process-{index}-{}", "n".repeat(24)).into(),
            pid: index.to_string().into(),
            cpu: text.clone(),
            cpu_kind: CellKind::Value,
            memory: bytes.clone(),
            memory_kind: CellKind::Value,
            disk: bytes.clone(),
            disk_kind: CellKind::Value,
            network: bytes.clone(),
            network_kind: CellKind::Value,
            gpu3d: text.clone(),
            gpu3d_kind: CellKind::Value,
            gpu_encode: text.clone(),
            gpu_encode_kind: CellKind::Value,
            gpu_decode: text.clone(),
            gpu_decode_kind: CellKind::Value,
            vram: bytes.clone(),
            vram_kind: CellKind::Value,
            shared: bytes,
            shared_kind: CellKind::Value,
        }
    }

    fn open_performance(width: f32, height: f32, dark: bool) -> AppWindow {
        backend();
        let ui = AppWindow::new().expect("window");
        ui.window().set_size(slint::LogicalSize::new(width, height));
        ui.set_page(1);
        ui.set_nav_expanded(true);
        ui.set_demo(true);
        ui.set_detail_graph(true);
        ui.set_detail_title("CPU".into());
        ui.set_detail_value("42.0%".into());
        ui.set_detail_extra("論理プロセッサ 8 個".into());
        ui.set_detail_note("計測の説明です。グラフと重ならないことを確認します。".into());
        ui.set_cpu_summary("42.0%".into());
        ui.set_memory_summary("6.4 GB / 16 GB".into());
        let points = ModelRc::from(Rc::new(VecModel::from(vec![
            10.0f32, 80.0, 30.0, 95.0, 20.0,
        ])));
        ui.set_detail_points(points.clone());
        ui.set_cpu_spark(points);
        ui.global::<Theme>()
            .set_preference(if dark { 1 } else { 0 });
        ui.show().expect("show");
        ui
    }

    fn assert_performance_fits(ui: &AppWindow, width: f32, height: f32) {
        let column = element(ui, "resource-column");
        let detail = element(ui, "detail-column");
        let title = element(ui, "detail-title");
        let value = element(ui, "detail-value");
        let graph = element(ui, "detail-graph");
        let column_size = column.size();
        let column_at = column.absolute_position();
        let detail_size = detail.size();
        let detail_at = detail.absolute_position();
        assert!(
            column_size.width > 160.0,
            "resource column collapsed to {}",
            column_size.width
        );
        assert!(
            column_size.height > 200.0 && column_size.height <= height + 1.0,
            "resource column height {} does not match window {height}",
            column_size.height
        );
        assert!(
            detail_size.width > 100.0,
            "detail column collapsed to {}",
            detail_size.width
        );
        assert!(
            detail_size.height <= height + 1.0,
            "detail column is taller than the window"
        );
        assert!(
            column_at.x + column_size.width <= detail_at.x + 2.0,
            "resource list overlaps the detail panel"
        );
        assert!(detail_at.x + detail_size.width <= width + 1.0);
        assert!(column_at.y >= -1.0 && column_at.y + column_size.height <= height + 1.0);
        assert!(title.size().width > 16.0 && title.size().height > 8.0);
        assert!(value.size().width > 16.0 && value.size().height > 8.0);
        assert!(graph.size().width > 60.0 && graph.size().height > 80.0);
        let graph_at = graph.absolute_position();
        assert!(graph_at.x >= detail_at.x - 1.0);
        assert!(graph_at.x + graph.size().width <= detail_at.x + detail_size.width + 2.0);
        assert!(
            value.absolute_position().y + value.size().height <= graph_at.y + 2.0,
            "the value overlaps the graph"
        );
        let cpu = ElementHandle::find_by_accessible_label(ui, "CPU")
            .next()
            .expect("CPU button");
        let memory = ElementHandle::find_by_accessible_label(ui, "メモリ")
            .next()
            .expect("memory button");
        assert!(
            cpu.absolute_position().y + cpu.size().height <= memory.absolute_position().y + 1.0
        );
        assert!(cpu.size().height > 40.0);
        memory.mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(ui.get_resource(), 1);
    }

    #[test]
    fn performance_page_fits_light_and_dark_at_three_sizes() {
        for dark in [false, true] {
            for (width, height) in [(1280.0, 720.0), (1920.0, 1080.0), (800.0, 600.0)] {
                let ui = open_performance(width, height, dark);
                assert_eq!(ui.global::<Theme>().get_dark(), dark);
                assert_performance_fits(&ui, width, height);
                let _ = ui.hide();
            }
        }
    }

    #[test]
    fn process_list_stays_virtualized_with_a_thousand_rows() {
        let ui = open_performance(1280.0, 720.0, false);
        let rows: Vec<ProcessRow> = (0..1000).map(sample_row).collect();
        ui.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_page(0);
        ui.show().expect("show process page");
        let list = element(&ui, "process-list");
        assert!(
            list.size().height < 2_000.0,
            "ListView height {} looks like it instantiated every row",
            list.size().height
        );
        assert!(list.size().height > 80.0);
        let before = ui.get_table_width();
        ui.global::<Columns>().set_name(420.0);
        let after = ui.get_table_width();
        assert!(after > before, "column resize did not grow the table width");
        let _ = ui.hide();
    }

    #[test]
    fn graph_stays_inside_the_detail_card() {
        for dark in [false, true] {
            for (width, height) in [(1280.0, 720.0), (1920.0, 1080.0), (800.0, 600.0)] {
                let ui = open_performance(width, height, dark);
                let graph = element(&ui, "detail-graph");
                let detail = element(&ui, "detail-column");
                let column = element(&ui, "resource-column");
                let graph_right = graph.absolute_position().x + graph.size().width;
                let detail_right = detail.absolute_position().x + detail.size().width;
                let graph_left = graph.absolute_position().x;
                let detail_left = detail.absolute_position().x;
                assert!(
                    graph_left >= detail_left + 8.0,
                    "{width}x{height} dark={dark}: graph left {graph_left} is not inset from {detail_left}"
                );
                assert!(
                    graph_right <= detail_right - 8.0,
                    "{width}x{height} dark={dark}: graph right {graph_right} passes the card edge {detail_right}"
                );
                assert!(
                    column.size().width + 1.0 >= 180.0,
                    "resource column {} is below the minimum",
                    column.size().width
                );
                assert!(
                    detail.size().width + 1.0 >= 240.0,
                    "detail card {} is below the minimum",
                    detail.size().width
                );
                assert!(column.absolute_position().x + column.size().width <= detail_left + 8.0);
                let _ = ui.hide();
            }
        }
    }

    #[test]
    fn hidden_column_drops_out_and_reorder_moves_the_header() {
        let ui = open_performance(1280.0, 720.0, false);
        ui.set_page(0);
        ui.show().expect("show");
        let before = ui.get_table_width();
        let mut prefs = crate::uiprefs::UiPrefs::default();
        prefs.toggle_column(4);
        let columns = ui.global::<Columns>();
        columns.set_shown(ModelRc::new(VecModel::from(prefs.column_shown.clone())));
        assert!(
            ui.get_table_width() < before - 100.0,
            "hiding disk did not shrink the table"
        );
        assert!(ElementHandle::find_by_accessible_label(&ui, "ディスク")
            .next()
            .is_none());
        prefs.move_column(2, -1);
        prefs.move_column(2, -1);
        columns.set_order(ModelRc::new(VecModel::from(prefs.column_order.clone())));
        let cpu = ElementHandle::find_by_accessible_label(&ui, "CPU")
            .next()
            .expect("cpu header");
        let name = ElementHandle::find_by_accessible_label(&ui, "名前")
            .next()
            .expect("name header");
        assert!(cpu.absolute_position().x < name.absolute_position().x);
        let _ = ui.hide();
    }

    #[test]
    fn hiding_the_selected_resource_keeps_a_visible_selection() {
        let ui = open_performance(1280.0, 720.0, false);
        let mut prefs = crate::uiprefs::UiPrefs::default();
        let next = prefs.toggle_resource(0, ui.get_resource());
        assert_eq!(next, 1);
        ui.set_resource(next);
        ui.set_detail_title("選択中".into());
        ui.set_resource_shown(ModelRc::new(VecModel::from(prefs.resource_shown)));
        ui.show().expect("show");
        assert!(ElementHandle::find_by_accessible_label(&ui, "CPU")
            .next()
            .is_none());
        assert!(ElementHandle::find_by_accessible_label(&ui, "メモリ")
            .next()
            .is_some());
        assert_eq!(ui.get_resource(), 1);
        let _ = ui.hide();
    }

    #[test]
    fn wider_resource_pref_moves_the_splitter() {
        let ui = open_performance(1280.0, 720.0, false);
        ui.set_resource_pref(320.0);
        let column = element(&ui, "resource-column");
        let detail = element(&ui, "detail-column");
        assert!(
            (column.size().width - 320.0).abs() < 4.0,
            "column width {}",
            column.size().width
        );
        assert!(detail.absolute_position().x >= column.absolute_position().x + column.size().width);
        let _ = ui.hide();
    }

    fn inside(inner: &ElementHandle, outer: &ElementHandle) -> bool {
        let at = inner.absolute_position();
        let size = inner.size();
        let outer_at = outer.absolute_position();
        let outer_size = outer.size();
        at.x >= outer_at.x - 1.0
            && at.y >= outer_at.y - 1.0
            && at.x + size.width <= outer_at.x + outer_size.width + 1.0
            && at.y + size.height <= outer_at.y + outer_size.height + 1.0
    }

    #[test]
    fn column_menu_stays_inside_the_window_and_holds_its_rows() {
        let ui = open_performance(1280.0, 720.0, false);
        ui.set_page(0);
        ui.set_column_menu_open(true);
        ui.show().expect("show");
        let menu = element(&ui, "column-menu");
        let at = menu.absolute_position();
        let size = menu.size();
        assert!(
            size.width >= 260.0 && size.height > 200.0,
            "column menu is {} x {}",
            size.width,
            size.height
        );
        assert!(at.x >= 0.0 && at.y >= 0.0);
        assert!(at.x + size.width <= 1280.0 + 1.0);
        assert!(at.y + size.height <= 720.0 + 1.0);
        let down = ElementHandle::find_by_accessible_label(&ui, "名前 を下へ")
            .next()
            .expect("name down button");
        assert!(
            inside(&down, &menu),
            "down button {:?} is outside the menu {:?} {:?}",
            (down.absolute_position(), down.size()),
            at,
            size
        );
        let button = ElementHandle::find_by_accessible_label(&ui, "資源の表示").next();
        assert!(
            button.is_none(),
            "resource menu button is on the process page"
        );
        let _ = ui.hide();
    }

    #[test]
    fn resource_menu_button_is_not_covered_by_the_scrollbar() {
        let ui = open_performance(800.0, 600.0, true);
        let button = ElementHandle::find_by_accessible_label(&ui, "資源の表示")
            .next()
            .expect("resource menu button");
        let column = element(&ui, "resource-column");
        let button_right = button.absolute_position().x + button.size().width;
        let column_right = column.absolute_position().x + column.size().width;
        assert!(
            button_right <= column_right - 14.0,
            "button right {button_right} meets the scrollbar at {column_right}"
        );
        assert!(button.size().width >= 48.0);
        ui.set_resource_menu_open(true);
        let menu = element(&ui, "resource-menu");
        let at = menu.absolute_position();
        let size = menu.size();
        assert!(size.width >= 240.0 && size.height > 160.0);
        assert!(at.x >= 0.0 && at.y >= 0.0);
        assert!(at.x + size.width <= 800.0 + 1.0);
        assert!(at.y + size.height <= 600.0 + 1.0);
        let up = ElementHandle::find_by_accessible_label(&ui, "CPU を下へ")
            .next()
            .expect("cpu down");
        assert!(inside(&up, &menu));
        let _ = ui.hide();
    }

    #[cfg(feature = "mock")]
    #[test]
    fn clicking_a_history_bar_selects_that_snapshot() {
        use crate::mock::{MockProbe, Scenario};
        use crate::pick::{index_to_fraction, select_slot};
        use crate::rank::{rank, RankStatus};
        use crate::sampler::SamplerCore;

        backend();
        let mut core = SamplerCore::new(Box::new(MockProbe::new(Scenario::History)), Vec::new());
        let mut sample = core.tick();
        for _ in 1..120 {
            sample = core.tick();
        }
        let axis = super::axis_samples(&sample.timeline);
        let window_ns = u64::try_from(crate::config::HISTORY_WINDOW.as_nanos()).unwrap();
        let slot =
            select_slot(&axis, index_to_fraction(axis.len(), 59), window_ns).expect("30s slot");
        let snapshot = sample.timeline[slot].as_ref().expect("recorded");
        let RankStatus::Ranked(rows) = rank(snapshot, ResourceId::Cpu, None, 5) else {
            panic!("ranking");
        };
        assert_eq!(rows[0].name, "spike.exe", "the 30s-ago spike should lead");
        assert!(
            rows.iter().any(|row| row.name == "gone.exe"),
            "the process that is still alive 30s ago stays in that ranking"
        );

        let ui = open_performance(1280.0, 720.0, false);
        let points = crate::series::scale_percent(&crate::series::graph_points(
            sample.timeline.iter().map(|slot| slot.as_deref()),
            sample.timeline.len(),
            ResourceId::Cpu,
            None,
        ))
        .points;
        ui.set_detail_points(ModelRc::new(VecModel::from(points)));
        ui.set_detail_graph(true);
        ui.show().expect("show");
        let clicked = Rc::new(std::cell::Cell::new(-1));
        let clicked_slot = Rc::clone(&clicked);
        ui.on_history_picked(move |index| clicked_slot.set(index));
        let mut bars: Vec<_> = ElementHandle::find_by_accessible_label(&ui, "履歴").collect();
        bars.sort_by(|left, right| {
            left.absolute_position()
                .x
                .partial_cmp(&right.absolute_position().x)
                .unwrap()
        });
        assert_eq!(bars.len(), 120, "one bar per history slot");
        bars[59].mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(clicked.get(), 59);
        let _ = ui.hide();
    }
}
