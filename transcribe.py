import sys, time
from faster_whisper import WhisperModel
src, model, offset, out = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]
prompt = ("محاضرة clinical pharmacokinetics: clearance, volume of distribution, half-life, "
          "elimination rate constant, AUC, bioavailability, loading dose, maintenance dose, "
          "steady state, creatinine clearance, Cockcroft-Gault, phenytoin, Michaelis-Menten, "
          "Vmax, Km, aminoglycosides, gentamicin, vancomycin, digoxin, theophylline, renal impairment.")
t = time.time()
m = WhisperModel(model, device="cpu", compute_type="int8", cpu_threads=4)
segs, info = m.transcribe(src, language="ar", initial_prompt=prompt, beam_size=5, vad_filter=True)
with open(out, "w") as f:
    f.write(f"# model={model} lang=ar\n")
    n = 0
    for s in segs:
        a = s.start + offset
        f.write(f"[{int(a//60):02d}:{int(a%60):02d}] {s.text.strip()}\n"); n += 1
    f.write(f"# segments={n} audio_s={info.duration:.0f} wall_s={time.time()-t:.0f}\n")
print("done", n, "segments, wall", round(time.time()-t), "s")
