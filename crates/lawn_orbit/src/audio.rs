use std::sync::Arc;

use kira::{
    AudioManager, AudioManagerSettings, DefaultBackend, Frame, Tween,
    sound::static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings},
};
use lawn_core::{
    profile::AudioSettings,
    run::{RunEvent, RunState},
};

pub struct AudioDirector {
    manager: AudioManager<DefaultBackend>,
    motor: StaticSoundHandle,
    mower: StaticSoundHandle,
    cutting: StaticSoundHandle,
    ambience: StaticSoundHandle,
    music: StaticSoundHandle,
    collision: StaticSoundData,
    rock_scrape: StaticSoundData,
    clipping: StaticSoundData,
    completion: StaticSoundData,
    boost: StaticSoundData,
    previous_boost: bool,
    cutting_envelope: f32,
}

impl std::fmt::Debug for AudioDirector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioDirector")
            .finish_non_exhaustive()
    }
}

impl AudioDirector {
    pub fn new() -> anyhow::Result<Self> {
        let mut manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())?;
        let motor_data = tone(92.0, 1.0, WaveKind::Motor)
            .loop_region(..)
            .volume(-24.0);
        let mower_data = tone(148.0, 1.0, WaveKind::Motor)
            .loop_region(..)
            .volume(-60.0);
        let cutting_data = tone(510.0, 1.0, WaveKind::Noise)
            .loop_region(..)
            .volume(-60.0);
        let ambience_data = tone(42.0, 2.0, WaveKind::Wind)
            .loop_region(..)
            .volume(-28.0);
        let music_data = tone(110.0, 4.0, WaveKind::Music)
            .loop_region(..)
            .volume(-60.0);
        let motor = manager.play(motor_data)?;
        let mower = manager.play(mower_data)?;
        let cutting = manager.play(cutting_data)?;
        let ambience = manager.play(ambience_data)?;
        let music = manager.play(music_data)?;
        Ok(Self {
            manager,
            motor,
            mower,
            cutting,
            ambience,
            music,
            collision: tone(78.0, 0.22, WaveKind::Impact),
            rock_scrape: tone(330.0, 0.18, WaveKind::Scrape),
            clipping: tone(690.0, 0.10, WaveKind::Clipping),
            completion: tone(523.25, 0.75, WaveKind::Chime),
            boost: tone(260.0, 0.28, WaveKind::Boost),
            previous_boost: false,
            cutting_envelope: 0.0,
        })
    }

    pub fn update(&mut self, run: &RunState, settings: &AudioSettings) {
        let master = settings.master.clamp(0.0, 1.0);
        let effects = settings.effects.clamp(0.0, 1.0) * master;
        let speed_ratio =
            (run.vehicle.state.speed() / run.vehicle_tuning.max_forward_speed).clamp(0.0, 1.3);
        self.motor
            .set_playback_rate(f64::from(0.72 + speed_ratio * 0.75), Tween::default());
        self.motor.set_volume(
            amplitude_db(effects * (0.15 + speed_ratio * 0.35)),
            Tween::default(),
        );
        self.mower
            .set_volume(amplitude_db(effects * 0.28), Tween::default());
        self.mower
            .set_playback_rate(f64::from(0.92 + speed_ratio * 0.12), Tween::default());
        let newly_cut = run
            .events()
            .iter()
            .any(|event| matches!(event, RunEvent::GrassCut { .. }));
        self.cutting_envelope = if newly_cut {
            (self.cutting_envelope + 0.28).min(1.0)
        } else {
            self.cutting_envelope * 0.84
        };
        self.cutting.set_volume(
            amplitude_db(effects * self.cutting_envelope * 0.42),
            Tween::default(),
        );
        self.ambience.set_volume(
            amplitude_db(settings.ambience * master * 0.22),
            Tween::default(),
        );
        self.music.set_volume(
            amplitude_db(settings.music * master * 0.13),
            Tween::default(),
        );
        if run.vehicle.state.boost_active && !self.previous_boost {
            let _ = self
                .manager
                .play(self.boost.volume(amplitude_db(effects * 0.7)));
        }
        self.previous_boost = run.vehicle.state.boost_active;
        for event in run.events() {
            match event {
                RunEvent::SubstantialCollision { impulse } => {
                    let volume = (impulse / 12.0).clamp(0.2, 1.0) * effects;
                    let _ = self
                        .manager
                        .play(self.collision.volume(amplitude_db(volume)));
                }
                RunEvent::RockScrape => {
                    let _ = self
                        .manager
                        .play(self.rock_scrape.volume(amplitude_db(effects * 0.42)));
                }
                RunEvent::GrassCut { weight } if *weight > 0.006 => {
                    let _ = self
                        .manager
                        .play(self.clipping.volume(amplitude_db(effects * 0.18)));
                }
                _ => {}
            }
        }
    }

    pub fn play_completion(&mut self, settings: &AudioSettings) {
        let _ = self.manager.play(
            self.completion
                .volume(amplitude_db(settings.master * settings.effects * 0.75)),
        );
    }
}

#[derive(Clone, Copy)]
enum WaveKind {
    Motor,
    Noise,
    Wind,
    Impact,
    Chime,
    Boost,
    Scrape,
    Clipping,
    Music,
}

fn tone(frequency: f32, seconds: f32, kind: WaveKind) -> StaticSoundData {
    const SAMPLE_RATE: u32 = 44_100;
    let count = (SAMPLE_RATE as f32 * seconds) as usize;
    let mut frames = Vec::with_capacity(count);
    let mut noise = 0x1234_5678_u32;
    for index in 0..count {
        let t = index as f32 / SAMPLE_RATE as f32;
        noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let random = ((noise >> 9) as f32 / (1_u32 << 23) as f32) * 2.0 - 1.0;
        let envelope = match kind {
            WaveKind::Impact => (1.0 - t / seconds).max(0.0).powi(3),
            WaveKind::Chime => (1.0 - t / seconds).max(0.0).powf(1.5),
            WaveKind::Boost => (1.0 - t / seconds).max(0.0),
            WaveKind::Scrape | WaveKind::Clipping => (1.0 - t / seconds).max(0.0).powi(2),
            _ => 1.0,
        };
        let sample = match kind {
            WaveKind::Motor => {
                ((t * frequency * std::f32::consts::TAU).sin()
                    + 0.3 * (t * frequency * 2.03 * std::f32::consts::TAU).sin())
                    * 0.32
            }
            WaveKind::Noise => random * 0.18 + (t * frequency * std::f32::consts::TAU).sin() * 0.06,
            WaveKind::Wind => random * 0.04 + (t * 0.23 * std::f32::consts::TAU).sin() * 0.025,
            WaveKind::Impact => {
                (random * 0.55 + (t * frequency * std::f32::consts::TAU).sin() * 0.4) * envelope
            }
            WaveKind::Chime => {
                let phase = t * frequency * std::f32::consts::TAU;
                (phase.sin() + (phase * 1.25).sin() * 0.55 + (phase * 1.5).sin() * 0.35)
                    * 0.28
                    * envelope
            }
            WaveKind::Boost => {
                ((t * (frequency + t * 620.0) * std::f32::consts::TAU).sin() * 0.38 + random * 0.08)
                    * envelope
            }
            WaveKind::Scrape => {
                (random * 0.32 + (t * frequency * std::f32::consts::TAU).sin() * 0.12) * envelope
            }
            WaveKind::Clipping => {
                (random * 0.25 + (t * frequency * std::f32::consts::TAU).sin() * 0.08) * envelope
            }
            WaveKind::Music => {
                let beat = (t * 0.5).fract();
                let soft = (1.0 - (beat - 0.22).abs() * 3.2).clamp(0.0, 1.0);
                let phase = t * frequency * std::f32::consts::TAU;
                (phase.sin() + (phase * 1.5).sin() * 0.38 + (phase * 2.0).sin() * 0.16)
                    * 0.12
                    * soft
            }
        };
        frames.push(Frame::from_mono(sample));
    }
    StaticSoundData {
        sample_rate: SAMPLE_RATE,
        frames: Arc::from(frames),
        settings: StaticSoundSettings::default(),
        slice: None,
    }
}

fn amplitude_db(amplitude: f32) -> f32 {
    if amplitude <= 0.001 {
        -60.0
    } else {
        (20.0 * amplitude.log10()).clamp(-60.0, 0.0)
    }
}
