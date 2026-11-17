const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync('crates/gm-openbot/src/companion_ui.js', 'utf8');
const start = source.indexOf('function parseSseFrames(');
const end = source.indexOf('async function connectEvents(', start);
const context = vm.createContext({});
vm.runInContext(source.slice(start, end), context);
const parse = context.parseSseFrames;

test('SSE handles every possible network split and CRLF multiline data', () => {
  const wire = ': keepalive\r\n\r\nevent: MESSAGE_ADDED\r\ndata: {"message":\r\ndata: "é→"}\r\n\r\n';
  for (let split = 0; split <= wire.length; split++) {
    const events = [];
    const dispatch = (type, data) => events.push({type, data});
    let buffer = parse(wire.slice(0, split), dispatch);
    buffer = parse(buffer + wire.slice(split), dispatch);
    assert.equal(buffer, '');
    assert.deepEqual(events, [{type: 'MESSAGE_ADDED', data: '{"message":\n"é→"}'}]);
  }
});
test('SSE ignores heartbeat frames and retains incomplete tails', () => {
  const events = [];
  const tail = parse(': heartbeat\n\ndata: one\n\ndata: two', (...args) => events.push(args));
  assert.deepEqual(events, [['message', 'one']]);
  assert.equal(tail, 'data: two');
  assert.throws(() => parse('x'.repeat(1024 * 1024 + 1), () => {}), /exceeds limit/);
});
