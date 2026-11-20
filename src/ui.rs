//! Slint window. This module only copies published snapshots onto the UI thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::config::UI_REFRESH_INTERVAL;
use crate::format::DisplayKind;
use crate::model::ResourceId;
use crate::present::{present_cards, present_detail, present_rows, status_line, PresentedRow};
use crate::sampler::{PublishedSample, SamplerHandle};
use crate::sort::SortColumn;

slint::include_modules!();

struct UiState {
    sort_column: SortColumn,
    sort_desc: bool,
    resource: ResourceId,
    last_sequence: Option<u64>,
    rows: Rc<VecModel<ProcessRow>>,
    cpu_points: Rc<VecModel<f32>>,
    memory_points: Rc<VecModel<f32>>,
}

pub fn run() -> Result<(), String> {
    let sampler = SamplerHandle::start()?;
    let ui = AppWindow::new().map_err(|err| format!("ウィンドウを作成できませんでした: {err}"))?;
    ui.set_version_text(format!("PULSE {}", env!("CARGO_PKG_VERSION")).into());
    // System mode follows AppsUseLightTheme. Missing key stays light.
    ui.global::<Theme>()
        .set_system_light(crate::platform::apps_use_light_theme().unwrap_or(true));

    let state = Rc::new(RefCell::new(UiState {
        sort_column: SortColumn::Cpu,
        sort_desc: true,
        resource: ResourceId::Cpu,
        last_sequence: None,
        rows: Rc::new(VecModel::from(Vec::<ProcessRow>::new())),
        cpu_points: Rc::new(VecModel::from(vec![-1.0f32; crate::history_capacity()])),
        memory_points: Rc::new(VecModel::from(vec![-1.0f32; crate::history_capacity()])),
    }));
    {
        let state = state.borrow();
        ui.set_rows(ModelRc::from(state.rows.clone()));
        ui.set_cpu_history(ModelRc::from(state.cpu_points.clone()));
        ui.set_memory_history(ModelRc::from(state.memory_points.clone()));
    }

    let latest = sampler.latest();
    wire_sort(&ui, &state, latest.clone());
    wire_resource(&ui, &state, latest.clone());

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
    let (column, descending, resource, rows_model, cpu_model, memory_model) = {
        let mut state = state.borrow_mut();
        if !force && state.last_sequence == Some(sample.snapshot.sequence) {
            return;
        }
        state.last_sequence = Some(sample.snapshot.sequence);
        (
            state.sort_column,
            state.sort_desc,
            state.resource,
            Rc::clone(&state.rows),
            Rc::clone(&state.cpu_points),
            Rc::clone(&state.memory_points),
        )
    };

    let presented = present_rows(&sample.snapshot, column, descending);
    sync_model(&rows_model, presented.into_iter().map(to_row).collect());
    sync_model(&cpu_model, to_points(&sample.cpu_history));
    sync_model(&memory_model, to_points(&sample.memory_history));

    ui.set_status_text(
        status_line(
            &sample.snapshot,
            sample.self_cpu,
            sample.self_memory,
            &sample.updated_text,
        )
        .into(),
    );
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

    let detail = present_detail(&sample.snapshot, resource);
    ui.set_detail_title(detail.title.into());
    ui.set_detail_value(detail.value.into());
    ui.set_detail_kind(detail.kind.code());
    ui.set_detail_extra(detail.extra.into());
    ui.set_detail_note(detail.note.into());
    ui.set_detail_graph(detail.show_graph);
    ui.set_detail_graph_memory(detail.graph_is_memory);
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

fn to_points(values: &[Option<f64>]) -> Vec<f32> {
    values
        .iter()
        .map(|value| match value {
            Some(number) if number.is_finite() => *number as f32,
            _ => -1.0,
        })
        .collect()
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
