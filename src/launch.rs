//! Command line for the window. Demo mode exists only in builds with the `mock` feature.

#[derive(Clone, Debug, PartialEq)]
pub struct LaunchOptions {
    pub demo: Option<String>,
    pub size: Option<(f32, f32)>,
    pub page: i32,
    /// 0 light, 1 dark, 2 system. `None` leaves the default (system).
    pub theme: Option<i32>,
    /// `columns` or `resources`. Opens that menu after the window is created.
    pub menu: Option<String>,
    pub hide_column: Option<String>,
    pub hide_resource: Option<String>,
    /// Performance resource index. `None` leaves CPU.
    pub resource: Option<i32>,
    /// One-shot history bar to pin after the first snapshot. Screenshot aid.
    pub select_history: Option<i32>,
    /// Mock seconds to publish after pinning, so the point leaves the ring.
    pub age_history: Option<u32>,
    /// Start on the per-logical-CPU view.
    pub cpu_view_logical: bool,
    /// Display index of the logical CPU to select once data exists.
    pub select_cpu: Option<i32>,
    /// `Some` forces detailed analysis on or off. `None` follows the demo.
    pub cpu_detail_on: Option<bool>,
}

impl LaunchOptions {
    pub fn from_env() -> Result<Self, String> {
        let mut options = Self {
            demo: None,
            size: None,
            page: 0,
            theme: None,
            menu: None,
            hide_column: None,
            hide_resource: None,
            resource: None,
            select_history: None,
            age_history: None,
            cpu_view_logical: false,
            select_cpu: None,
            cpu_detail_on: None,
        };
        let mut cpu_view_set = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--demo" {
                let name = args.next().ok_or_else(|| {
                    "--demo の後に normal、many、gaps、special、history、nics のいずれかが必要です"
                        .to_string()
                })?;
                #[cfg(feature = "mock")]
                {
                    crate::mock::Scenario::parse(&name).ok_or_else(|| {
                        format!("未知のデモシナリオです: {name}（normal / p500 / many / p2000 / gaps / special / history / cpus / cpus32 / cpus-denied / nics）")
                    })?;
                    options.demo = Some(name);
                }
                #[cfg(not(feature = "mock"))]
                {
                    let _ = name;
                    return Err(
                        "このビルドに DEMO DATA は入っていません。--features mock で起動してください。"
                            .to_string(),
                    );
                }
            } else if arg == "--size" {
                let text = args
                    .next()
                    .ok_or_else(|| "--size の後に 1280x720 のような寸法が必要です".to_string())?;
                options.size = Some(parse_size(&text)?);
            } else if arg == "--page" {
                let text = args.next().ok_or_else(|| {
                    "--page の後に process または performance が必要です".to_string()
                })?;
                options.page = match text.as_str() {
                    "process" | "0" => 0,
                    "performance" | "1" => 1,
                    other => {
                        return Err(format!(
                            "未知のページです: {other}（process または performance）"
                        ))
                    }
                };
            } else if arg == "--theme" {
                let text = args.next().ok_or_else(|| {
                    "--theme の後に light、dark、system のいずれかが必要です".to_string()
                })?;
                options.theme = Some(match text.as_str() {
                    "light" | "0" => 0,
                    "dark" | "1" => 1,
                    "system" | "2" => 2,
                    other => {
                        return Err(format!(
                            "未知のテーマです: {other}（light / dark / system）"
                        ))
                    }
                });
            } else if arg == "--menu" {
                let text = args.next().ok_or_else(|| {
                    "--menu の後に columns または resources が必要です".to_string()
                })?;
                if text != "columns" && text != "resources" {
                    return Err(format!(
                        "未知のメニューです: {text}（columns または resources）"
                    ));
                }
                options.menu = Some(text);
            } else if arg == "--hide-column" {
                let text = args
                    .next()
                    .ok_or_else(|| "--hide-column の後に列名が必要です".to_string())?;
                options.hide_column = Some(text);
            } else if arg == "--hide-resource" {
                let text = args
                    .next()
                    .ok_or_else(|| "--hide-resource の後に資源名が必要です".to_string())?;
                options.hide_resource = Some(text);
            } else if arg == "--resource" {
                let text = args.next().ok_or_else(|| {
                    "--resource の後に cpu / memory / disk / network / gpu3d / gpu-encode / gpu-decode / vram / shared が必要です".to_string()
                })?;
                let resource = crate::model::ResourceId::parse(&text)
                    .ok_or_else(|| format!("未知の資源です: {text}"))?;
                options.resource = Some(resource as i32);
            } else if arg == "--select-history" {
                let text = args
                    .next()
                    .ok_or_else(|| "--select-history の後に棒の番号が必要です".to_string())?;
                let index: i32 = text
                    .parse()
                    .map_err(|_| format!("棒の番号が整数ではありません: {text}"))?;
                if index < 0 {
                    return Err("棒の番号は 0 以上です".to_string());
                }
                options.select_history = Some(index);
            } else if arg == "--age-history" {
                let text = args
                    .next()
                    .ok_or_else(|| "--age-history の後に秒数が必要です".to_string())?;
                let seconds: u32 = text
                    .parse()
                    .map_err(|_| format!("秒数が整数ではありません: {text}"))?;
                if seconds == 0 {
                    return Err("--age-history は 1 秒以上です".to_string());
                }
                options.age_history = Some(seconds);
            } else if arg == "--cpu-view" {
                let text = args.next().ok_or_else(|| {
                    "--cpu-view の後に overall または logical が必要です".to_string()
                })?;
                options.cpu_view_logical = match text.as_str() {
                    "overall" | "0" => false,
                    "logical" | "1" => true,
                    other => {
                        return Err(format!(
                            "未知の CPU 表示です: {other}（overall または logical）"
                        ))
                    }
                };
                cpu_view_set = true;
            } else if arg == "--select-cpu" {
                let text = args
                    .next()
                    .ok_or_else(|| "--select-cpu の後に番号が必要です".to_string())?;
                let index: i32 = text
                    .parse()
                    .map_err(|_| format!("CPU 番号が整数ではありません: {text}"))?;
                if index < 0 {
                    return Err("CPU 番号は 0 以上です".to_string());
                }
                options.select_cpu = Some(index);
            } else if arg == "--cpu-detail" {
                let text = args
                    .next()
                    .ok_or_else(|| "--cpu-detail の後に on または off が必要です".to_string())?;
                options.cpu_detail_on = Some(match text.as_str() {
                    "on" | "1" => true,
                    "off" | "0" => false,
                    other => return Err(format!("未知の詳細分析です: {other}（on または off）")),
                });
            } else if arg == "--help" || arg == "-h" {
                return Err(help_text());
            } else {
                return Err(format!("未知の引数です: {arg}\n{}", help_text()));
            }
        }
        if options.age_history.is_some() && options.demo.is_none() {
            return Err("--age-history は --demo と一緒に指定します".to_string());
        }
        if options.age_history.is_some() && options.select_history.is_none() {
            return Err("--age-history には --select-history も必要です".to_string());
        }
        if options.cpu_detail_on.is_none()
            && options
                .demo
                .as_deref()
                .is_some_and(|name| matches!(name, "cpus" | "cpus32" | "cpus-denied"))
        {
            options.cpu_detail_on = Some(true);
        }
        if !cpu_view_set
            && options
                .demo
                .as_deref()
                .is_some_and(|name| matches!(name, "cpus" | "cpus32" | "cpus-denied"))
        {
            options.cpu_view_logical = true;
        }
        Ok(options)
    }

    /// Screenshot plan: keep the chosen slot, then move the mock clock forward.
    pub fn retained_pin(&self) -> Option<crate::sampler::RetainRequest> {
        let seconds = self.age_history?;
        let index = usize::try_from(self.select_history?).ok()?;
        let step_ms = crate::config::CPU_SAMPLE_INTERVAL.as_millis().max(1);
        let steps = (u128::from(seconds) * 1000 / step_ms).max(1);
        let extra_steps = u32::try_from(steps).unwrap_or(u32::MAX);
        Some(crate::sampler::RetainRequest { index, extra_steps })
    }
}

fn parse_size(text: &str) -> Result<(f32, f32), String> {
    let (width, height) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("寸法の形式が違います: {text}"))?;
    let width: f32 = width
        .parse()
        .map_err(|_| format!("幅が数値ではありません: {width}"))?;
    let height: f32 = height
        .parse()
        .map_err(|_| format!("高さが数値ではありません: {height}"))?;
    if width < 800.0 || height < 560.0 {
        return Err("最小サイズは 800x560 です".to_string());
    }
    Ok((width, height))
}

fn help_text() -> String {
    "[--size 1080x720] [--page process|performance] [--theme light|dark|system] [--resource cpu|memory|disk|network|gpu3d|gpu-encode|gpu-decode|vram|shared] [--select-history N] [--age-history 秒] [--cpu-view overall|logical] [--select-cpu N] [--cpu-detail on|off] [--menu columns|resources] [--hide-column disk] [--hide-resource gpu-encode] [--demo normal|p500|many|p2000|gaps|special|history|cpus|cpus32|cpus-denied|nics]\n--demo は mock feature 付きの開発ビルドだけが受け付けます。--size を省くと 1080x720 で開きます。--select-history は 60 秒軸の棒を一度だけ固定します。--age-history はその点を固定したあと、モックの時計だけを進め、リングから外れた選択を残します。--cpu-view logical は論理プロセッサ別のグラフです。--cpu-detail は ETW の詳細分析です。列と資源の表示、ナビの開閉は %APPDATA%\\PULSE\\ui.cfg（Linux では ~/.config/PULSE/ui.cfg）に保存します。`nav=` が無いファイルは折りたたみです。".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_rejects_a_window_smaller_than_the_minimum() {
        assert!(parse_size("640x480").is_err());
        assert_eq!(parse_size("1280x720").unwrap(), (1280.0, 720.0));
    }
}
