#!/usr/bin/env python3
"""Generate bounded autoregressive input/reference pairs; no hardware access."""
import argparse
import hashlib
import json
from pathlib import Path
import numpy as np
import torch
from transformers import LlamaConfig, LlamaForCausalLM
from safetensors.torch import load_file
import sentencepiece as spm

ap=argparse.ArgumentParser(description=__doc__)
ap.add_argument('--prepared',type=Path,required=True)
ap.add_argument('--tokenizer',type=Path,required=True)
ap.add_argument('--output',type=Path,required=True)
ap.add_argument('--prompt',default='Once upon a time')
ap.add_argument('--steps',type=int,default=3)
args=ap.parse_args()
args.output.mkdir(exist_ok=False,parents=True)
torch.set_num_threads(2)
config=json.loads((args.prepared/'hf-model/config.json').read_text())
model=LlamaForCausalLM(LlamaConfig(**config)).float().eval()
weights=load_file(str(args.prepared/'hf-model/model.safetensors'))
model.load_state_dict(weights,strict=True)
embeddings=weights['model.embed_tokens.weight'].half().numpy().astype('<f2')
embeddings.tofile(args.output/'embeddings.bin')
tokenizer=spm.SentencePieceProcessor(model_file=str(args.tokenizer))
prompt=[1]+tokenizer.encode(args.prompt)
if not 1 <= args.steps <= 32 or len(prompt)+args.steps-1 > 32:
    ap.error('prompt and requested steps must fit the 32-position model')
np.asarray(prompt,dtype='<u4').tofile(args.output/'prompt-tokens.bin')
tokens=list(prompt)
samples=[]
with torch.no_grad():
    for index in range(args.steps):
        padded=tokens+[0]*(32-len(tokens))
        inputs=embeddings[padded].T.copy()
        logits=model(torch.tensor([padded]),use_cache=False).logits[0].numpy().T.copy()
        predicted=int(logits[:,len(tokens)-1].argmax())
        inputs.tofile(args.output/f'{index}.bin')
        logits.astype('<f4').tofile(args.output/f'{index}-cpu-logits.bin')
        samples.append(dict(index=index,context_tokens=list(tokens),context_length=len(tokens),
                            next_token=predicted,input_sha256=hashlib.sha256(inputs.tobytes()).hexdigest()))
        tokens.append(predicted)
result=dict(prompt_tokens=prompt,generated_tokens=tokens,text=tokenizer.decode(tokens),
            samples=samples,linux_ane_tested=False,reference='HF FP32 with trained weights; ANE receives FP16 embeddings')
(args.output/'manifest.json').write_text(json.dumps(result,indent=2)+'\n')
np.asarray([s['next_token'] for s in samples],dtype='<u4').tofile(args.output/'next-tokens.bin')
print(json.dumps({k:result[k] for k in ['prompt_tokens','generated_tokens','text']}))
