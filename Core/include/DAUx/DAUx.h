/*
 * DAUx/DAUx.h - Umbrella header for the public C ABI.
 */
#ifndef DAUX_DAUX_H
#define DAUX_DAUX_H

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Abi/Version.h>
#include <DAUx/Abi/Types.h>

#include <DAUx/Audio/SampleFormat.h>
#include <DAUx/Audio/AudioBuffer.h>
#include <DAUx/Audio/AudioBus.h>
#include <DAUx/Audio/ChannelLayout.h>
#include <DAUx/Audio/ProcessContext.h>

#include <DAUx/Event/Event.h>
#include <DAUx/Event/EventQueue.h>

#include <DAUx/Automation/Automation.h>

#include <DAUx/Plugin/Category.h>
#include <DAUx/Plugin/Capabilities.h>
#include <DAUx/Plugin/Descriptor.h>
#include <DAUx/Plugin/Lifecycle.h>
#include <DAUx/Plugin/EntryPoint.h>
#include <DAUx/Plugin/Plugin.h>

#include <DAUx/Parameter/ParameterFlags.h>
#include <DAUx/Parameter/ParameterValue.h>
#include <DAUx/Parameter/ParameterInfo.h>
#include <DAUx/Parameter/Parameter.h>

#include <DAUx/State/StateBlob.h>
#include <DAUx/State/State.h>

#include <DAUx/Program/Program.h>
#include <DAUx/Program/Preset.h>
#include <DAUx/Program/ProgramList.h>

#include <DAUx/Component/ProcessData.h>
#include <DAUx/Component/Processor.h>
#include <DAUx/Component/Controller.h>
#include <DAUx/Component/ComponentState.h>
#include <DAUx/Component/Component.h>

#include <DAUx/Editor/NativeWindow.h>
#include <DAUx/Editor/EditorBounds.h>
#include <DAUx/Editor/Editor.h>

#include <DAUx/Gui/GuiFramework.h>
#include <DAUx/Gui/GuiMessage.h>
#include <DAUx/Gui/GuiSurface.h>
#include <DAUx/Gui/GuiView.h>
#include <DAUx/Gui/GuiHost.h>

#include <DAUx/Host/HostCallbacks.h>
#include <DAUx/Host/HostEvents.h>
#include <DAUx/Host/HostServices.h>
#include <DAUx/Host/Host.h>

#include <DAUx/Factory/PluginClassInfo.h>
#include <DAUx/Factory/PluginFactory.h>
#include <DAUx/Factory/PluginClassRegistry.h>

#endif /* DAUX_DAUX_H */
