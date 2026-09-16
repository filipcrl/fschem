"""IR stage schemas are machine-readable snapshots of the actual compiler seams."""
from pathlib import Path
import json
import subprocess
import pytest
import fschem

ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture
def circuit():
    return fschem.Project(ROOT, discover_libraries=False).load('examples/portable.sch')


@pytest.mark.parametrize('stage', ['parsed', 'resolved', 'lowered'])
def test_deterministic_read_only_ir(circuit, stage, monkeypatch):
    def forbidden(*args, **kwargs):
        raise AssertionError('IR inspection must not execute subprocesses')
    monkeypatch.setattr(subprocess, 'Popen', forbidden)
    config = {'parameters': {'R': '10k', 'VDD': 3}} if stage == 'lowered' else {}
    first = circuit._native.dump_ir(stage, 'ngspice', json.dumps(config))
    second = circuit._native.dump_ir(stage, 'ngspice', json.dumps(config))
    assert first == second
    data = json.loads(first)
    assert data == circuit.dump_ir(stage, config=config)
    assert data['schema_version'] == 1 and data['stage'] == stage
    assert data['source'] == str(circuit.path)
    assert 'deck' not in data
    data['source'] = 'mutated detached object'
    assert circuit.dump_ir(stage, config=config)['source'] == str(circuit.path)


def test_parsed_lossless_and_properties(circuit):
    parsed = circuit.dump_ir('parsed')
    assert parsed['text'] == circuit.source
    r = next(r for r in parsed['records'] if r['kind'] == 'instance')
    assert r['symbol'] == 'resistor.sym'
    assert r['properties']['entries'][0] == {'key': 'name', 'value': 'R1'}
    raw = circuit.source.encode()[r['span'][0]:r['span'][1]].decode()
    assert raw == r['raw'] and r['line'] > 0
    assert r['id'] == f"record:{r['span'][0]}"


def test_resolved_ordered_pins(circuit):
    ir = circuit.dump_ir('resolved')
    r = next(r for r in ir['instances'] if r['id'] == '/R1')
    assert [(p['order'], p['name'], p['net']) for p in r['pins']] == [(0, 'P', 'VIN'), (1, 'N', 'VOUT')]
    assert r['source'] == str(circuit.path) and r['line'] > 0
    assert ir['sheets'][0]['nets'] == ['0', 'VIN', 'VOUT']


def test_lowered_structure_matches_serialized_deck(circuit):
    sim = fschem.Simulation(circuit, mode='portable', parameters={'R': '10k', 'VDD': 3}).op()
    ir = sim.dump_ir()
    assert [d['id'] for d in ir['devices']] == ['/R1', '/R2', '/V1']
    r = ir['devices'][0]
    assert r['kind'] == 'resistor' and r['model'] == 'resistor'
    assert r['parameters'] == {'r': 10000.0}
    assert r['expressions']['value'] == '{R}'
    assert [t['node'] for t in r['terminals']] == ['VIN', 'VOUT']
    assert ir['devices'][2]['waveform'] == {'kind': 'dc', 'value': 3.0}
    assert ir['scopes'][0]['effective_parameters'] == {'R': 10000.0, 'VDD': 3.0}
    assert ir['signal_map']['/VOUT'] == 'VOUT'
    deck = sim.prepare(backend='ngspice').deck
    assert f"{r['emitted_name']} VIN VOUT 1e4" in deck


@pytest.mark.parametrize('stage,config,match', [
    ('bad', {}, 'unknown IR stage'),
    ('parsed', {'parameters': {'R': 1}}, 'only for lowered'),
    ('lowered', {'typo': 1}, 'unknown field'),
    ('lowered', {'parameters': {'R': True}}, 'strings or numbers'),
    ('lowered', {'exclude': '*'}, 'sequence'),
    ('lowered', {'sources': {'/V': {'kind': 'voltage', 'nodes': ['/a', '/0'], 'value': 1, 'extra': 2}}}, 'unknown field'),
])
def test_invalid_ir_options_fail_before_execution(circuit, stage, config, match):
    with pytest.raises(ValueError, match=match):
        circuit.dump_ir(stage, config=config)
    with pytest.raises(ValueError, match='invalid IR config'):
        circuit._native.dump_ir('lowered', 'ngspice', 'not json')


def test_incomplete_parsed_stage_fails(tmp_path):
    source = tmp_path / 'bad.sch'
    source.write_text('v {xschem version=3.4.8 file_version=1.3}\nZ {unsupported}\n')
    c = fschem.Project(tmp_path, discover_libraries=False).load('bad.sch')
    with pytest.raises(ValueError, match='parsed IR unavailable'):
        c.dump_ir('parsed')
