"""Local OpenAI-compatible provider for desktop integration testing."""
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def send_json(self, data, status=200):
        body = json.dumps(data, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == '/v1/models':
            if self.headers.get('Authorization') != 'Bearer desktop-fixture-key':
                self.send_json({'error': {'message': 'Invalid fixture key'}}, 401)
                return
            self.send_json({'data': [{'id': 'classroom-test'}, {'id': 'whisper-1'}]})
        else:
            self.send_json({'error': {'message': 'Unknown fixture route'}}, 404)

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        if self.headers.get('Authorization') != 'Bearer desktop-fixture-key':
            self.send_json({'error': {'message': 'Invalid fixture key'}}, 401)
            return
        if self.path != '/v1/chat/completions':
            self.send_json({'error': {'message': 'Unknown fixture route'}}, 404)
            return
        data = json.loads(raw)
        messages = data.get('messages', [])
        if not messages or not data.get('model'):
            self.send_json({'error': {'message': 'Missing conversation or model'}}, 400)
            return
        content = '\n'.join(str(m.get('content', '')) for m in messages)
        reply = '## 课堂验收\n\n已收到本地课堂上下文。\n\n- 关键知识：水的沸点与气压有关。\n- 复习问题：为什么高海拔地区水更容易沸腾？\n'
        if '100' in content:
            reply += '- 材料读取成功：100摄氏度。\n'
        if not data.get('stream'):
            self.send_json({'choices': [{'message': {'role': 'assistant', 'content': reply}}]})
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Cache-Control', 'no-cache')
        self.end_headers()
        try:
            for offset in range(0, len(reply), 5):
                event = {'choices': [{'delta': {'content': reply[offset:offset + 5]}}]}
                self.wfile.write(('data: ' + json.dumps(event, ensure_ascii=False) + '\n\n').encode())
                self.wfile.flush()
                time.sleep(0.08)
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


if __name__ == '__main__':
    server = ThreadingHTTPServer(('127.0.0.1', 18743), Handler)
    print('Desktop provider fixture listening on port 18743', flush=True)
    server.serve_forever()
