"""Hermetic portable circuit/scoping fixtures (no PDK model redistribution)."""
from pathlib import Path
import shutil

import numpy as np
import pytest
import fschem

ROOT = Path(__file__).resolve().parents[2]
HEADER = 'v {xschem version=3.4.8 file_version=1.3}\nG {}\nK {}\nV {}\nS {}\nE {}\n'


def symbol(root, name, kind, pins, defaults):
    root.joinpath(name + '.sym').write_text(
        HEADER.replace('K {}', f'K {{type={kind} template="{defaults}"}}') + ''.join(
            f'B 5 {i*20-1} -1 {i*20+1} 1 {{name={pin}}}\n' for i, pin in enumerate(pins)))


def circuit(root, instances):
    """instances = (symbol, properties, node labels); horizontally spaced rows."""
    text = HEADER
    for row, (sym, props, nodes) in enumerate(instances):
        y = row * 100
        text += f'C {{{sym}.sym}} 0 {y} 0 0 {{{props}}}\n'
        text += ''.join(f'N {i*20} {y} {i*20+5} {y} {{lab={n}}}\n'
                        for i, n in enumerate(nodes))
    root.joinpath('top.sch').write_text(text)
    return fschem.Project(root, discover_libraries=False).load('top.sch')


def compile_raw(circuit, backend='ngspice', parameters=None, locals=None, exclude=(), sources=None):
    return circuit._native.portable(backend, parameters or {}, locals or {}, set(exclude), sources or {})[0]


@pytest.mark.parametrize('backend', ['ngspice', 'vacask'])
def test_primitives_multiplicity_and_waveforms(tmp_path, backend):
    for sym, kind in [('r', 'resistor'), ('c', 'capacitor'), ('l', 'inductor'), ('v', 'vsource')]:
        symbol(tmp_path, sym, kind, ['P', 'N'], f'name={sym} value=1' + (' m=1' if sym != 'v' else ''))
    c = circuit(tmp_path, [('r', 'name=R value=12 m=3', ['IN', 'OUT']),
                           ('c', 'name=C value=2p m=4', ['OUT', '0']),
                           ('l', 'name=L value=8n m=2', ['OUT', '0']),
                           ('v', 'name=V value="PULSE(0 1 {T} 1p 2p {2*T} {4*T})"', ['IN', '0']),
                           ('v', 'name=V2 value="PWL(0 0 {T} 1 {2*T} 0)"', ['OTHER', '0'])])
    text = compile_raw(c, backend, {'T': '1n'})
    for expected in ('4e0', '8e-12', '4e-9', '2e-9'):
        assert expected in text
    assert ('PULSE(' if backend == 'ngspice' else 'type="pulse"') in text
    assert ('PWL(' if backend == 'ngspice' else 'wave=[') in text


def test_hierarchy_defaults_overrides_and_locals(tmp_path):
    symbol(tmp_path, 'cell', 'subcircuit', ['P'], 'name=X a=1k b=2*a')
    symbol(tmp_path, 'r', 'resistor', ['P', 'N'], 'name=R value=1 m=1')
    symbol(tmp_path, 'port', 'ipin', ['P'], 'name=p lab=P')
    tmp_path.joinpath('cell.sch').write_text(HEADER +
        'C {r.sym} 0 0 0 0 {name=R value={b+extra}}\n'
        'C {port.sym} 0 0 0 0 {name=p lab=P}\nN 20 0 25 0 {lab=0}\n')
    c = circuit(tmp_path, [('cell', 'name=X1 a={TOP}', ['A']), ('cell', 'name=X2', ['B'])])
    locals = {'/X1': {'extra': '1k'}, '/X2': {'extra': '2k'}}
    text = compile_raw(c, parameters={'TOP': '3k'}, locals=locals)
    assert ' A 0 7e3' in text
    assert ' B 0 4e3' in text
    with pytest.raises(ValueError, match='shadow'):
        compile_raw(c, parameters={'TOP': '3k'}, locals={'/X1': {'a': '2k'}})
    with pytest.raises(ValueError, match='undefined'):
        compile_raw(c, parameters={'TOP': 'missing'}, locals=locals)
    with pytest.raises(ValueError, match='unknown local'):
        compile_raw(c, parameters={'TOP': '3k'}, locals={**locals, '/typo': {}})
    with pytest.raises(ValueError, match='cyclic'):
        compile_raw(c, parameters={'TOP': 'TOP+1'}, locals=locals)


@pytest.mark.parametrize('backend', ['ngspice', 'vacask'])
@pytest.mark.parametrize('polarity', ['n', 'p'])
def test_ihp_named_pins_and_symbol_defaults(tmp_path, backend, polarity):
    model = f'sg13_lv_{polarity}mos'
    # Intentionally shuffled terminals; handcrafted metadata, not a PDK copy.
    symbol(tmp_path, model, polarity + 'mos', ['B', 'S', 'D', 'G'],
           f'name=M model={model} spiceprefix=X w=1u l=0.13u ng=2 m=3 mm_ok=1')
    c = circuit(tmp_path, [(model, 'name=M', ['BULK', 'SOURCE', 'DRAIN', 'GATE'])])
    text = compile_raw(c, backend)
    assert 'DRAIN GATE SOURCE BULK' in text
    assert model in text and 'ng=2e0' in text and 'm=3e0' in text and 'mm_ok=1e0' in text
    assert text.startswith('x')
    c = circuit(tmp_path, [(model, 'name=M mm_ok=0 m=4 ng=5', ['BULK', 'SOURCE', 'DRAIN', 'GATE'])])
    text = compile_raw(c, backend)
    assert 'mm_ok=0e0' in text and 'm=4e0' in text and 'ng=5e0' in text


@pytest.mark.parametrize('props,match', [('value="SIN(0 1 1k)"', 'expression'),
                                       ('value={missing}', 'undefined'),
                                       ('value=1 ac=1', 'unsupported property'),
                                       ('value="PWL(1 0 0 1)"', 'increase'),
                                       ('value=1 spice_ignore=true', 'backend-specific')])
def test_reject_unsupported(tmp_path, props, match):
    symbol(tmp_path, 'v', 'vsource', ['P', 'N'], 'name=V value=1')
    c = circuit(tmp_path, [('v', 'name=V ' + props, ['IN', '0'])])
    with pytest.raises(ValueError, match=match):
        compile_raw(c)


def test_native_conflict_exact_exclusions(tmp_path):
    symbol(tmp_path, 'code', 'netlist_commands', [], 'name=PARAMS value=.op')
    c = circuit(tmp_path, [('code', 'name=PARAMS value=".param x=1"', [])])
    with pytest.raises(ValueError, match='native code conflict at /PARAMS'):
        compile_raw(c)
    assert compile_raw(c, exclude=['/PARAMS']) == ''
    assert compile_raw(c, exclude=[str(c.path) + '#PARAMS']) == ''
    with pytest.raises(ValueError, match='matched no native block'):
        compile_raw(c, exclude=['/PARAMS', '/typo'])


@pytest.mark.parametrize('backend', ['ngspice', 'vacask'])
def test_behavioral_and_supplemental_sources(tmp_path, backend):
    symbol(tmp_path, 'v', 'vsource', ['P', 'N'], 'name=V value=1')
    symbol(tmp_path, 'b', 'source', ['P', 'N'], 'name=B VAR=V FUNC=1 m=1')
    c = circuit(tmp_path, [('v', 'name=V value="PULSE(0 1 0 1p 1p 1n 2n)"', ['CLK', '0']),
                           ('b', 'name=B FUNC="v(CLK)*v(CLK_EN)"', ['GATED', '0'])])
    original = c.path.read_bytes()
    sources = {'/VCLKEN': {'kind': 'voltage', 'nodes': ['/CLK_EN', '/0'],
                           'value': 'PWL(0 1 {TSTOP} 1 {TSTOP+TEDGE} 0)'}}
    s = fschem.Simulation(c, mode='portable', parameters={'TSTOP': '3n', 'TEDGE': '1n'}, sources=sources).op().tran(stop=5e-9, output_step=1e-11)
    p = s.prepare(backend=backend)
    assert 'v(CLK) * v(CLK_EN)' in p.deck
    assert '4e-9' in p.deck and '3.0000000000000004e-9' in p.deck
    assert ('CLK_EN 0 PWL' if backend == 'ngspice' else '(CLK_EN 0) fschem_vsource type="pwl"') in p.deck
    run = p.write(tmp_path / 'prepared')
    assert run.manifest['portable']['sources']['/VCLKEN']['nodes'] == ['/CLK_EN', '/0']
    assert c.path.read_bytes() == original
    if shutil.which(backend):
        home = tmp_path / 'home'
        home.mkdir()
        run.execute(timeout=30, env={'HOME': str(home)})
        tran = run.result('tran')
        assert abs(tran.voltage('/GATED')[-1]) < 1e-8
        assert np.max(tran.voltage('/GATED')) > 0.9
    for bad, match in [({'/v': sources['/VCLKEN']}, 'collision'),
                       ({'/X/VCLKEN': sources['/VCLKEN']}, 'root-only'),
                       ({'/VCLKEN': {**sources['/VCLKEN'], 'kind': 'opaque'}}, 'unsupported supplemental')]:
        with pytest.raises(ValueError, match=match):
            fschem.Simulation(c, mode='portable', parameters=dict(TSTOP='3n', TEDGE='1n'), sources=bad).op().prepare(backend=backend)


@pytest.mark.parametrize('backend', ['ngspice', 'vacask'])
def test_real_portable_divider_and_immutable_override(tmp_path, backend):
    if not shutil.which(backend):
        pytest.skip('engine unavailable')
    c = fschem.Project(ROOT, discover_libraries=False).load('examples/vacask.sch')
    s = fschem.Simulation(c, mode='portable', exclude=['/SIM'], parameters={'unused': 1}).op().tran(stop=1e-8, output_step=1e-9)
    other = s.with_parameters(unused=2)
    assert s.parameters['unused'] == '1' and other.parameters['unused'] == '2'
    other.op(name='second_op')
    assert len(s._analyses) == 2
    p = s.prepare(backend=backend)
    s.op(name='later')  # Prepared metadata must be a snapshot.
    home = tmp_path / 'home'
    home.mkdir()
    run = p.run(directory=tmp_path / 'run', timeout=20, env={'HOME': str(home)})
    assert run.manifest['mode'] == 'portable'
    assert len(run.manifest['analyses']['specifications']) == 2
    reopened = fschem.Run.open(run.directory)
    assert np.allclose(reopened.result('op').voltage('/VOUT'), 2)
    assert reopened.result('tran').time[-1] == pytest.approx(1e-8)


def test_prepare_guards():
    c = fschem.Project(ROOT, discover_libraries=False).load('examples/vacask.sch')
    s = fschem.Simulation(c, mode='portable', exclude=['/SIM'])
    with pytest.raises(ValueError, match='at least one'):
        s.prepare(backend='ngspice')
    with pytest.raises(ValueError, match='positive'):
        s.tran(stop=-1, output_step=1)
    s.op(save=['/missing'])
    with pytest.raises(ValueError, match='unknown save'):
        s.prepare(backend='ngspice')
    with pytest.raises(ValueError, match='duplicate'):
        s.op()
