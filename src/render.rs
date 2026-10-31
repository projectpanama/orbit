use smithay::backend::renderer::{
    ImportAll, ImportMem,
    element::{
        memory::MemoryRenderBufferRenderElement, solid::SolidColorRenderElement,
        surface::WaylandSurfaceRenderElement, utils::RescaleRenderElement,
    },
};

smithay::backend::renderer::element::render_elements! {
    pub OrbitRenderElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<R>,
    RescaledSurface=RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    RescaledSolid=RescaleRenderElement<SolidColorRenderElement>,
    RescaledMemory=RescaleRenderElement<MemoryRenderBufferRenderElement<R>>,
}
