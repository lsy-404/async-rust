# Desktop contract
Commands use camelCase argument keys and returned structs.
- load_state() -> AppData {workspaces: Workspace[], sessions: Session[], materials: Material[], settings: Settings, providers: Provider[]}
- create_workspace({name}) -> Workspace {id,name}
- delete_workspace({id}) -> void (cascade)
- create_session({workspaceId,title}) -> Session {id,workspaceId,title,messages:Message[],transcription,summary}
- delete_session({id}) -> void
- save_session({session: Session}) -> void
- import_material({workspaceId,path}) -> Material {id,workspaceId,name,content,path}
- save_material({material: Material}) -> void
- delete_material({id}) -> void
- save_settings({settings: Settings}) -> void
Settings {providerId,model,transcriptionModel,theme:'system'|'light'|'dark',language:'zh'|'en'}
Provider {id,name,baseUrl,models:string[],hasKey:boolean}
- save_provider({provider:Provider,apiKey:string|null}) -> Provider
- delete_provider({id}) -> void
- discover_models({providerId}) -> string[]
- chat({sessionId,content,onEvent:Channel<StreamEvent>}) -> void
- cancel_generation({sessionId}) -> void
- summarize({sessionId,onEvent:Channel<StreamEvent>}) -> void
StreamEvent {type:'delta'|'done',text:string}; errors reject command. Backend persists user and assistant message and summary, frontend reloads after command.
Message {id,role:'user'|'assistant'|'system',content}
- transcribe_audio({sessionId,path}) -> string (persists transcription; OpenAI-compatible audio/transcriptions)
- transcribe_bytes({sessionId,name,bytes:number[]}) -> string (recorded browser audio)
Defaults: provider 'openai', name 'OpenAI', baseUrl 'https://api.openai.com/v1', models []; BYOK user can edit endpoint. No fake/sample data.
Backend model context includes session transcription + workspace material content with size bounds.
