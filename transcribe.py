import sys, time
from faster_whisper import WhisperModel
audio, model_dir, offset, out = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]
m = WhisperModel(model_dir, device="cpu", compute_type="int8", cpu_threads=4)
segs, info = m.transcribe(audio, language="ar", task="transcribe", beam_size=5,
    condition_on_previous_text=False, vad_filter=True,
    vad_parameters=dict(min_silence_duration_ms=700),
    no_repeat_ngram_size=4, repetition_penalty=1.1,
    compression_ratio_threshold=2.2, log_prob_threshold=-1.0, no_speech_threshold=0.6,
    temperature=[0.0, 0.2, 0.4, 0.6])
def ts(s):
    s = int(s + offset); return f"{s//3600:02d}:{s%3600//60:02d}:{s%60:02d}"
with open(out, "w") as f:
    for s in segs:
        f.write(f"[{ts(s.start)} --> {ts(s.end)}] {s.text.strip()}\n"); f.flush()
