# VST3 Architecture Documentation

## Core Concepts

### Module Architecture
- **Object-oriented, cross-platform component model** primarily in C++
- **Factory Pattern**: Each module exports `GetPluginFactory()` function
- **Platform-specific entry/exit functions**:
  - Windows: Optional `InitDll`/`ExitDll`
  - macOS: Required `bundleEntry`/`bundleExit`  
  - Linux: Required `ModuleEntry`/`ModuleExit`
- **Interface versioning**: "Once an interface has been released, it must never change"
- **Unicode support**: UTF-16 encoding required from version 5 onwards

### Key Interfaces

#### IComponent (Processor)
- Provides plugin information
- Manages bus configurations
- Handles plugin activation
- Supports state storage/restoration
- Processes audio in blocks

#### IEditController
- Manages GUI and parameters
- Responsible for parameter changes
- Optional editor view creation
- Communicates parameter interactions to host

#### IPlugView
- Plugin's graphical user interface
- Platform-specific attachment (NSView, HWND, X11)
- Methods: `attached()`, `removed()`, `getSize()`, `onSize()`, `canResize()`
- Host provides IPlugFrame for resize requests

### Initialization Sequence

1. **Create Component**: Host creates processor component first via factory
2. **Initialize Component**: Call `IPluginBase::initialize()`
3. **Activate Buses**: Enable required audio and event buses
4. **Get/Create Controller**: 
   - Try casting component to IEditController (single component)
   - Or create separate controller via `getControllerClassId()`
5. **Initialize Controller**: If separate, call `initialize()` on controller
6. **Connect Components**: Use IConnectionPoint if separate
7. **Transfer State**: Sync component state to controller
8. **Activate Component**: Call `setActive(1)` for parameter access
9. **Setup Processing**: Configure audio processing parameters
10. **Start Processing**: Begin audio/MIDI processing

### Component/Controller Separation

**Single Component Model**:
- Component implements both IComponent and IEditController
- Simpler architecture, common for basic plugins

**Separate Component Model**:
- Distinct processor and controller instances
- Connected via IConnectionPoint
- Allows different threading models
- More flexible but complex

### Threading Model
- **UI Thread**: Initialization, parameter changes, GUI
- **Audio Thread**: Processing, real-time operations
- **Critical**: Avoid memory allocation in audio thread

## Audio Processing

### Process Setup
```cpp
ProcessSetup {
    processMode: kRealtime,
    symbolicSampleSize: kSample32,
    maxSamplesPerBlock: blockSize,
    sampleRate: 44100.0
}
```

### Process Data Structure
- Contains audio buffers, events, parameter changes
- ProcessContext with timing/transport information
- Input/output event lists for MIDI
- Parameter change queues

### Bus Configuration
- Audio buses (mono, stereo, surround)
- Event buses (MIDI input/output)
- Must be activated before processing
- Dynamic speaker arrangements supported

## MIDI in VST3

VST3 doesn't use raw MIDI - instead uses high-resolution events:

### Event Types
- **NoteOnEvent**: pitch (int16), velocity (float32), channel (int16)
- **NoteOffEvent**: pitch (int16), velocity (float32), channel (int16)
- **PolyPressureEvent**: pitch (int16), pressure (float32)
- **DataEvent**: For SysEx and other data
- **ControllerEvent**: Mapped to parameters

### Key Differences from MIDI
- 32-bit float velocity (vs 7-bit MIDI)
- 32-bit float pressure (vs 7-bit MIDI)
- 64-bit double parameters (vs 7-14 bit MIDI)
- Note IDs for per-note expression

## Parameters

### Characteristics
- **Normalized values**: Always 0.0 to 1.0
- **Unique 32-bit IDs**: Each parameter has unique identifier
- **Flags**: kCanAutomate, kIsBypass, kIsReadOnly, etc.
- **Step count**: For discrete values
- **Units and titles**: Display strings

### Parameter Updates
```cpp
// From UI to processor
controller->beginEdit(id)
controller->performEdit(id, value) 
controller->endEdit(id)

// During processing
processor->getParameterChanges()
```

### Automation
- Host records parameter changes
- Plugin signals manipulation via begin/perform/endEdit
- No automated parameter should influence another
- Host updates plugin GUI during playback

## GUI/View Creation

### IPlugView Lifecycle

1. **Creation**: `controller->createView("editor")`
2. **Size Query**: `view->getSize(&rect)`
3. **Platform Check**: `view->isPlatformTypeSupported(type)`
4. **Attachment**: `view->attached(parentWindow, platformType)`
5. **Resizing**: Via IPlugFrame callbacks
6. **Removal**: `view->removed()` when closing

### Platform Types
- macOS: `"NSView"` - Cocoa NSView pointer
- Windows: `"HWND"` - Win32 window handle
- Linux: `"X11EmbedWindowID"` - X11 window ID

### Resizing
- **Host-initiated**: User drags window, plugin validates via `checkSizeConstraint()`
- **Plugin-initiated**: Plugin calls `IPlugFrame::resizeView()`

## Best Practices

1. **Always initialize in correct order** - component before controller
2. **Activate buses before processing** - especially event buses for MIDI
3. **Handle both single and separate component models**
4. **Clear event lists each process cycle**
5. **Never allocate memory in audio thread**
6. **Properly clean up views** - call `removed()` when done
7. **Check return codes** - many operations can fail silently
8. **State transfer is critical** - sync component/controller states