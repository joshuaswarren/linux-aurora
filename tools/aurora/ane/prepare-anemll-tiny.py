#!/usr/bin/env python3
"""Prepare and check a pinned TinyStories model through ANEMLL; no device IO.

Weights are loaded with PyTorch's restricted weights-only reader. The HF
reference and ANEMLL graph use the same trained weights; this script does
not interpret CPU/Core ML execution as proof of Linux ANE execution.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

import numpy as np
import torch
from safetensors.torch import save_file
from transformers import LlamaConfig as HFConfig, LlamaForCausalLM as HFModel
import sentencepiece as spm


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--anemll', type=Path, required=True)
    ap.add_argument('--assets', type=Path, required=True)
    ap.add_argument('--output', type=Path, required=True)
    ap.add_argument('--convert', action='store_true')
    ap.add_argument('--stateless', action='store_true',
                    help='Export ANEMLL transformer/head with a full 32-token embedding input')
    args = ap.parse_args()
    torch.set_num_threads(2)
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((args.assets / 'download-manifest.json').read_text())
    assert manifest['revision'] == '0bd21da7698eaf29a0d7de3992de8a46ef624add'
    for entry in manifest['files']:
        data = (args.assets / entry['file']).read_bytes()
        assert len(data) == entry['bytes']
        assert hashlib.sha256(data).hexdigest() == entry['sha256']
    checkpoint = torch.load(args.assets / 'stories260K.pt', map_location='cpu',
                            weights_only=True)
    p = checkpoint['model_args']
    assert (p['dim'], p['n_layers'], p['n_heads'], p['n_kv_heads'], p['vocab_size']) == (64, 5, 8, 4, 512)
    original = {k.removeprefix('_orig_mod.'): v for k, v in checkpoint['model'].items()}
    config = dict(hidden_size=64, intermediate_size=172, num_hidden_layers=5,
                  num_attention_heads=8, num_key_value_heads=4, vocab_size=512,
                  max_position_embeddings=512, context_length=32, state_length=32,
                  rms_norm_eps=1e-5, rope_theta=10000., bos_token_id=1,
                  eos_token_id=2, hidden_act='silu', attention_bias=False,
                  tie_word_embeddings=True, model_type='llama')
    hf_weights = {'model.embed_tokens.weight': original['tok_embeddings.weight'],
                  'model.norm.weight': original['norm.weight'],
                  'lm_head.weight': original['output.weight']}
    assert torch.equal(hf_weights['model.embed_tokens.weight'], hf_weights['lm_head.weight'])
    mapping = {'attention.wq':'self_attn.q_proj', 'attention.wk':'self_attn.k_proj',
               'attention.wv':'self_attn.v_proj', 'attention.wo':'self_attn.o_proj',
               'feed_forward.w1':'mlp.gate_proj', 'feed_forward.w2':'mlp.down_proj',
               'feed_forward.w3':'mlp.up_proj', 'attention_norm':'input_layernorm',
               'ffn_norm':'post_attention_layernorm'}
    for i in range(5):
        for old, new in mapping.items():
            w = original[f'layers.{i}.{old}.weight']
            # llama2.c stores adjacent real/imaginary RoPE pairs; HF/ANEMLL
            # store the real half followed by the imaginary half per head.
            if old in ('attention.wq', 'attention.wk'):
                heads = 8 if old.endswith('wq') else 4
                w = w.reshape(heads, 4, 2, 64).transpose(1, 2).reshape(-1, 64)
            hf_weights[f'model.layers.{i}.{new}.weight'] = w.contiguous()
    hf = HFModel(HFConfig(**config)).float().eval()
    hf.load_state_dict(hf_weights, strict=True)
    model_dir = args.output / 'hf-model'
    model_dir.mkdir(exist_ok=True)
    (model_dir / 'config.json').write_text(json.dumps(config, indent=2) + '\n')
    save_file({k:v.clone().contiguous() for k,v in hf_weights.items()},
              str(model_dir / 'model.safetensors'))
    sys.path.insert(0, str(args.anemll.resolve()))
    from anemll.models import llama_model as am
    am.ENABLE_VACAB_SPLIT = False
    am.ENABLE_VACAB_SPLIT8 = False
    model = am.LlamaForCausalLM(am.LlamaConfig(**config)).eval()
    assert model.load_pretrained_weights(str(model_dir))
    model = model.half().eval()
    tokenizer = spm.SentencePieceProcessor(model_file=str(args.assets / 'tok512.model'))
    tokens = [1] + tokenizer.encode('Once upon a time')
    prompt = list(tokens)
    comparisons = []
    with torch.no_grad():
        # Consume the prompt, then greedily decode with the independent FP32
        # HF reference while checking ANEMLL's cached FP16 path at each step.
        for pos in range(min(28, len(prompt) + 12)):
            x = torch.tensor([[tokens[pos]]], dtype=torch.int64)
            mask = torch.full((1,1,1,32), -65504., dtype=torch.float16)
            mask[:,:,:,:pos+1] = 0
            result = model(x, torch.zeros((1,),dtype=torch.float16),
                           torch.tensor([pos]), torch.tensor([pos]), mask)[0,0].float()
            ref = hf(torch.tensor([tokens[:pos+1]]), use_cache=False).logits[0,-1]
            diff = (result-ref).abs()
            comparisons.append(dict(position=pos, max_abs_error=float(diff.max()),
                                    rms_error=float(diff.square().mean().sqrt()),
                                    reference_argmax=int(ref.argmax()),
                                    anemll_argmax=int(result.argmax())))
            if pos+1 >= len(tokens):
                tokens.append(int(ref.argmax()))
            if tokens[-1] == 2 and pos+1 >= len(prompt):
                break
    receipt = {'anemll_revision':subprocess.check_output(['git','-C',str(args.anemll),'rev-parse','HEAD'],text=True).strip(),
               'model_source':manifest, 'parameters':sum(x.numel() for x in hf.parameters()),
               'config':config,'prompt_tokens':prompt,'generated_tokens':tokens,
               'text':tokenizer.decode(tokens), 'comparisons':comparisons,
               'argmax_matches':sum(x['reference_argmax']==x['anemll_argmax'] for x in comparisons),
               'steps':len(comparisons),'linux_ane_tested':False}
    (args.output/'reference.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print('REFERENCE',json.dumps({k:receipt[k] for k in ['parameters','text','argmax_matches','steps']}))
    if any(not np.isfinite(x['max_abs_error']) for x in comparisons):
        raise SystemExit('Nonfinite ANEMLL parity result')
    if receipt['argmax_matches'] != receipt['steps']:
        raise SystemExit('ANEMLL/HF token mismatch; inspect reference.json before conversion')
    if args.convert:
        import coremltools as ct
        from anemll.ane_converter.llama_converter import LlamaConverter
        model.model.kv_cache_0.zero_()
        if args.stateless:
            class Stateless(torch.nn.Module):
                def __init__(self, original):
                    super().__init__()
                    self.model = original
                    self.register_buffer('positions', torch.arange(32,dtype=torch.int32))
                    self.register_buffer('mask',torch.triu(torch.full((1,1,32,32),-65504.,dtype=torch.float16),diagonal=1))
                    self.register_buffer('start',torch.zeros((1,),dtype=torch.int32))

                def forward(self, embeddings):
                    hidden = embeddings.squeeze(2).transpose(1,2)
                    rotary = self.model.model.get_rotary_embedding_prefill(self.positions)
                    # ANEMLL's cache-only prefill intentionally skips the
                    # final MLP. For logits, run every original attention,
                    # normalization and MLP module, using this full context
                    # directly instead of a persistent KV-cache buffer.
                    for layer in self.model.model.layers:
                        normalized = layer.input_layernorm(hidden)
                        q,k,v = layer.self_attn.get_new_kv_cache_prefill(
                            normalized,self.start,rotary,32)
                        attended = layer.self_attn.forward_prefill(normalized,q,
                            (k.squeeze(0),v.squeeze(0)),self.mask)
                        hidden = hidden + attended
                        hidden = hidden + layer.mlp(layer.post_attention_layernorm(hidden))
                    hidden = self.model.model.norm(hidden)
                    return self.model.lm_head1(hidden.transpose(1,2).unsqueeze(2))

            wrapper = Stateless(model).half().eval()
            for parameter in wrapper.parameters():
                parameter.requires_grad_(False)
            padded = (tokens + [0] * 32)[:32]
            with torch.no_grad():
                embeddings = model.embed_tokens(torch.tensor([padded])).half().transpose(1,2).unsqueeze(2)
                output = wrapper(embeddings)
                ref = hf(torch.tensor([padded]),use_cache=False).logits.transpose(1,2).unsqueeze(2)
                error = (output.float()-ref).abs()
                compare = dict(max_abs_error=float(error.max()),
                               argmax_matches=int((output.argmax(1)==ref.argmax(1)).sum()),
                               positions=32, input_shape=list(embeddings.shape),
                               output_shape=list(output.shape), linux_ane_tested=False)
                (args.output/'stateless-reference.json').write_text(json.dumps(compare,indent=2)+'\n')
                print('STATELESS_REFERENCE',json.dumps(compare))
                if compare['argmax_matches'] != 32:
                    raise SystemExit('Stateless ANEMLL/HF token mismatch')
                embeddings.numpy().astype('<f2').tofile(args.output/'stateless-input.bin')
                output.numpy().astype('<f2').tofile(args.output/'stateless-output-pytorch.bin')
                traced = torch.jit.trace(wrapper, embeddings)
            converted = ct.convert(traced,
                inputs=[ct.TensorType(name='input',shape=embeddings.shape,dtype=np.float16)],
                outputs=[ct.TensorType(name='output',dtype=np.float16)],
                minimum_deployment_target=ct.target.iOS18,
                compute_units=ct.ComputeUnit.CPU_AND_NE,
                compute_precision=ct.precision.FLOAT16,convert_to='mlprogram')
            path=args.output/'stories260k-stateless.mlpackage'
            converted.save(str(path))
            print('COREML_PACKAGE',path)
            return
        converter = LlamaConverter(model, context_length=32, state_length=32,
                                   batch_size=1, lut_bits=None)
        converted = converter.convert_monolithic(model)
        path = args.output / 'stories260k-stateful.mlpackage'
        converted.save(str(path))
        print('COREML_PACKAGE',path)


if __name__ == '__main__':
    main()
