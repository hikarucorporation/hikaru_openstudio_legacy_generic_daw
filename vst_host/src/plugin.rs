// SPDX-License-Identifier: LGPL-3.0-or-later
use log::{warn, info};
use std::sync::{mpsc, Arc};
use std::fmt;
use vst3_sys::base::{kResultOk, IPluginBase};
use vst3_sys::vst::{IAudioProcessor, IComponent, IComponentHandler, IEditController, ProcessSetup, SymbolicSampleSizes};
use vst3_sys::utils::SharedVstPtr;
use vst3_sys::{ComInterface, VstPtr};
use crate::component_handler::ComponentHandler;
use crate::module::Module;
use crate::plugin_descriptor::PluginDescriptor;
use crate::shared::Shared;
use crate::MainThreadMessage;

pub struct Plugin {
    pub descriptor: Arc<PluginDescriptor>,
    pub component: VstPtr<dyn IComponent>,
    pub processor: Option<VstPtr<dyn IAudioProcessor>>,
    pub edit_controller: Option<VstPtr<dyn IEditController>>,
    pub shared: Arc<Shared>,
    // Retenido para que el `ComponentHandler` no se libere: el plugin solo
    // guarda un puntero COM crudo hacia él (vía `SharedVstPtr`), no un `Box`
    // propio. Si soltamos este `Box` antes que el plugin, el puntero que
    // sostiene queda colgando.
    _component_handler: Box<ComponentHandler>,
}

impl fmt::Debug for Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plugin").field("name", &self.descriptor.name).finish()
    }
}

impl Plugin {
    pub fn load(module: &Module, descriptor: Arc<PluginDescriptor>) -> Result<Self, String> {
        info!("Cargando plugin: {}", descriptor.name);
        let component_ptr = crate::factory::create_instance(module, &descriptor.class_id);
        if component_ptr.is_null() {
            return Err(format!("{}: No se pudo instanciar el componente", descriptor.name));
        }

        let component: VstPtr<dyn IComponent> = unsafe {
            VstPtr::owned(component_ptr as *mut *mut _)
                .ok_or_else(|| format!("{}: Error al instanciar VstPtr para el componente", descriptor.name))?
        };

        let init_res = unsafe { component.initialize(std::ptr::null_mut()) };
        if init_res != kResultOk {
            return Err(format!(
                "{}: Fallo initialize() en el componente (code {init_res})",
                descriptor.name
            ));
        }

        let processor = component.cast::<dyn IAudioProcessor>();
        match &processor {
            Some(_) => info!("Procesador de audio enlazado con éxito para {}", descriptor.name),
            None => warn!("{}: El componente no expone la interfaz de procesamiento de audio", descriptor.name),
        }

        // El sender queda huérfano por ahora: nadie del lado GUI todavía
        // instancia un `Vst3Host` que drene el receiver correspondiente.
        // Igual que en ClapHost, ese receiver se conecta cuando exista
        // Vst3Host::plugin_add.
        let (sender, _receiver) = mpsc::channel::<MainThreadMessage>();
        let shared = Arc::new(Shared::new((*descriptor).clone(), sender));

        let edit_controller = component.cast::<dyn IEditController>();
        let component_handler = ComponentHandler::new(Arc::clone(&shared));

        match &edit_controller {
            Some(edit_controller) => {
                let handler_ptr = component_handler.as_ref() as *const ComponentHandler
                    as *mut *mut <dyn IComponentHandler as ComInterface>::VTable;

                // SAFETY: `handler_ptr` apunta al campo `vtable` de nuestro
                // `ComponentHandler`, que es `#[repr(C)]` con la vtable como
                // primer campo — el mismo layout que exige `ComInterface`.
                // `SharedVstPtr` es `#[repr(transparent)]` sobre exactamente
                // este puntero, así que el transmute preserva el layout.
                let shared_ptr: SharedVstPtr<dyn IComponentHandler> =
                    unsafe { std::mem::transmute(handler_ptr) };

                let res = unsafe { edit_controller.set_component_handler(shared_ptr) };
                if res != kResultOk {
                    warn!(
                        "{}: set_component_handler() devolvió {res}, el plugin puede no recibir restart_component",
                        descriptor.name
                    );
                }
            }
            None => warn!(
                "{}: el componente no expone IEditController, no se puede instalar IComponentHandler",
                descriptor.name
            ),
        }

        Ok(Self {
            descriptor,
            component,
            processor,
            edit_controller,
            shared,
            _component_handler: component_handler,
        })
    }

    pub fn prepare_to_play(&self, sample_rate: f64, max_samples_per_block: i32) -> Result<(), String> {
        if let Some(ref processor) = self.processor {
            let mut setup = ProcessSetup {
                process_mode: 0,
                symbolic_sample_size: SymbolicSampleSizes::kSample32 as _,
                max_samples_per_block,
                sample_rate,
            };

            let res = unsafe { processor.setup_processing(&mut setup) };
            if res != kResultOk {
                return Err(format!("Fallo setup_processing en {}: {}", self.descriptor.name, res));
            }
        }
        Ok(())
    }
}
