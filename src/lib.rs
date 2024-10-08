#![doc(html_root_url = "https://docs.rs/bevy_video_glitch/0.2.0")]
#![doc = include_str!("../README.md")]
use bevy::{
    asset::load_internal_asset,
    core_pipeline::{
        core_2d::graph::{Core2d, Node2d},
        core_3d::graph::{Core3d, Node3d},
        fullscreen_vertex_shader::fullscreen_shader_vertex_state,
    },
    ecs::query::QueryItem,
    prelude::*,
    render::{
        extract_component::{
            ComponentUniforms, ExtractComponent, ExtractComponentPlugin, UniformComponentPlugin,
        },
        globals::{GlobalsBuffer, GlobalsUniform},
        render_graph::{
            NodeRunError, RenderGraphApp, RenderGraphContext, RenderLabel, ViewNode, ViewNodeRunner,
        },
        render_resource::{
            binding_types::{sampler, texture_2d, uniform_buffer},
            BindGroupEntries, BindGroupLayout, BindGroupLayoutEntries, CachedRenderPipelineId,
            ColorTargetState, ColorWrites, FragmentState, MultisampleState, Operations,
            PipelineCache, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor,
            RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
            ShaderType, TextureFormat, TextureSampleType,
        },
        renderer::{RenderContext, RenderDevice},
        texture::BevyDefault,
        view::ViewTarget,
        RenderApp,
    },
};

// $ cargo install uuid-tools && uuid -o simple
pub const VIDEO_GLITCH_SHADER_HANDLE: Handle<Shader> =
    Handle::weak_from_u128(0x7b1d58197dc34e26b0c69a3c8091a014u128);

pub struct VideoGlitchPlugin;

impl Plugin for VideoGlitchPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            VIDEO_GLITCH_SHADER_HANDLE,
            "../assets/shaders/video-glitch.wgsl",
            Shader::from_wgsl
        );
        app.register_type::<VideoGlitchSettings>().add_plugins((
            // The settings will be a component that lives in the main world but will
            // be extracted to the render world every frame.
            // This makes it possible to control the effect from the main world.
            // This plugin will take care of extracting it automatically.
            // It's important to derive [`ExtractComponent`] on [`VideoGlitchSettings`]
            // for this plugin to work correctly.
            ExtractComponentPlugin::<VideoGlitchSettings>::default(),
            // The settings will also be the data used in the shader.
            // This plugin will prepare the component for the GPU by creating a uniform buffer
            // and writing the data to that buffer every frame.
            UniformComponentPlugin::<VideoGlitchSettings>::default(),
        ));

        // We need to get the render app from the main app
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            // Bevy's renderer uses a render graph which is a collection of nodes in a directed acyclic graph.
            // It currently runs on each view/camera and executes each node in the specified order.
            // It will make sure that any node that needs a dependency from another node
            // only runs when that dependency is done.
            //
            // Each node can execute arbitrary work, but it generally runs at least one render pass.
            // A node only has access to the render world, so if you need data from the main world
            // you need to extract it manually or with the plugin like above.
            // Add a [`Node`] to the [`RenderGraph`]
            // The Node needs to impl FromWorld
            //
            // The [`ViewNodeRunner`] is a special [`Node`] that will automatically run the node for each view
            // matching the [`ViewQuery`]
            .add_render_graph_node::<ViewNodeRunner<VideoGlitchNode>>(
                // Specify the name of the graph, in this case we want the graph for 3d
                Core3d,
                // It also needs the name of the node
                VideoGlitchLabel,
            )
            .add_render_graph_edges(
                Core3d,
                // Specify the node ordering.
                // This will automatically create all required node edges to enforce the given ordering.
                (
                    Node3d::Tonemapping,
                    VideoGlitchLabel,
                    Node3d::EndMainPassPostProcessing,
                ),
            )
            .add_render_graph_node::<ViewNodeRunner<VideoGlitchNode>>(Core2d, VideoGlitchLabel)
            .add_render_graph_edges(
                Core2d,
                (Node2d::EndMainPass, VideoGlitchLabel, Node2d::Tonemapping),
            );
    }

    fn finish(&self, app: &mut App) {
        // We need to get the render app from the main app
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            // Initialize the pipeline
            .init_resource::<VideoGlitchPipeline>();
    }
}
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct VideoGlitchLabel;

// The post process node used for the render graph
#[derive(Default)]
struct VideoGlitchNode;

// The ViewNode trait is required by the ViewNodeRunner
impl ViewNode for VideoGlitchNode {
    // The node needs a query to gather data from the ECS in order to do its rendering,
    // but it's not a normal system so we need to define it manually.
    //
    // This query will only run on the view entity
    type ViewQuery = &'static ViewTarget;

    // Runs the node logic
    // This is where you encode draw commands.
    //
    // This will run on every view on which the graph is running.
    // If you don't want your effect to run on every camera,
    // you'll need to make sure you have a marker component as part of [`ViewQuery`]
    // to identify which camera(s) should run the effect.
    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        view_target: QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        // Get the pipeline resource that contains the global data we need
        // to create the render pipeline
        let video_glitch_pipeline = world.resource::<VideoGlitchPipeline>();

        // The pipeline cache is a cache of all previously created pipelines.
        // It is required to avoid creating a new pipeline each frame,
        // which is expensive due to shader compilation.
        let pipeline_cache = world.resource::<PipelineCache>();

        // Get the pipeline from the cache
        let Some(pipeline) = pipeline_cache.get_render_pipeline(video_glitch_pipeline.pipeline_id)
        else {
            return Ok(());
        };

        // Get the settings uniform binding
        let settings_uniforms = world.resource::<ComponentUniforms<VideoGlitchSettings>>();
        let Some(settings_binding) = settings_uniforms.uniforms().binding() else {
            return Ok(());
        };

        let globals_buffer = world.resource::<GlobalsBuffer>();
        let Some(global_uniforms) = globals_buffer.buffer.binding() else {
            return Ok(());
        };

        // This will start a new "post process write", obtaining two texture
        // views from the view target - a `source` and a `destination`.
        // `source` is the "current" main texture and you _must_ write into
        // `destination` because calling `post_process_write()` on the
        // [`ViewTarget`] will internally flip the [`ViewTarget`]'s main
        // texture to the `destination` texture. Failing to do so will cause
        // the current main texture information to be lost.
        let post_process = view_target.post_process_write();

        // The bind_group gets created each frame.
        //
        // Normally, you would create a bind_group in the Queue set,
        // but this doesn't work with the post_process_write().
        // The reason it doesn't work is because each post_process_write will alternate the source/destination.
        // The only way to have the correct source/destination for the bind_group
        // is to make sure you get it during the node execution.
        let bind_group = render_context.render_device().create_bind_group(
            "video_glitch_bind_group",
            &video_glitch_pipeline.layout,
            // It's important for this to match the BindGroupLayout defined in the VideoGlitchPipeline
            &BindGroupEntries::sequential((
                // Make sure to use the source view
                post_process.source,
                // Use the sampler created for the pipeline
                &video_glitch_pipeline.sampler,
                // Set the settings binding
                settings_binding.clone(),
                global_uniforms,
            )),
        );

        // Begin the render pass
        let mut render_pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("video_glitch_pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                // We need to specify the post process destination view here
                // to make sure we write to the appropriate texture.
                view: post_process.destination,
                resolve_target: None,
                ops: Operations::default(),
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // This is mostly just wgpu boilerplate for drawing a fullscreen triangle,
        // using the pipeline/bind_group created above
        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, &bind_group, &[]);
        render_pass.draw(0..3, 0..1);

        Ok(())
    }
}

// This contains global data used by the render pipeline. This will be created once on startup.
#[derive(Resource)]
struct VideoGlitchPipeline {
    layout: BindGroupLayout,
    sampler: Sampler,
    pipeline_id: CachedRenderPipelineId,
}

impl FromWorld for VideoGlitchPipeline {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();

        let layout = render_device.create_bind_group_layout(
            "video_glitch_bind_group_layout",
            &BindGroupLayoutEntries::sequential(
                // The layout entries will only be visible in the fragment stage
                ShaderStages::FRAGMENT,
                (
                    // The screen texture
                    texture_2d(TextureSampleType::Float { filterable: true }),
                    // The sampler that will be used to sample the screen texture
                    sampler(SamplerBindingType::Filtering),
                    // The settings uniform that will control the effect
                    uniform_buffer::<VideoGlitchSettings>(false),
                    uniform_buffer::<GlobalsUniform>(false),
                ),
            ),
        );
        // // We need to define the bind group layout used for our pipeline
        // let layout = render_device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        //     label: Some("video_glitch_bind_group_layout"),
        //     entries: &[
        //         // The screen texture
        //         BindGroupLayoutEntry {
        //             binding: 0,
        //             visibility: ShaderStages::FRAGMENT,
        //             ty: BindingType::Texture {
        //                 sample_type: TextureSampleType::Float { filterable: true },
        //                 view_dimension: TextureViewDimension::D2,
        //                 multisampled: false,
        //             },
        //             count: None,
        //         },
        //         // The sampler that will be used to sample the screen texture
        //         BindGroupLayoutEntry {
        //             binding: 1,
        //             visibility: ShaderStages::FRAGMENT,
        //             ty: BindingType::Sampler(SamplerBindingType::Filtering),
        //             count: None,
        //         },
        //         // The settings uniform that will control the effect
        //         BindGroupLayoutEntry {
        //             binding: 2,
        //             visibility: ShaderStages::FRAGMENT,
        //             ty: BindingType::Buffer {
        //                 ty: bevy::render::render_resource::BufferBindingType::Uniform,
        //                 has_dynamic_offset: false,
        //                 min_binding_size: Some(VideoGlitchSettings::min_size()),
        //             },
        //             count: None,
        //         },
        //         // Globals
        //         BindGroupLayoutEntry {
        //             binding: 3,
        //             visibility: ShaderStages::FRAGMENT,
        //             ty: BindingType::Buffer {
        //                 ty: bevy::render::render_resource::BufferBindingType::Uniform,
        //                 has_dynamic_offset: false,
        //                 min_binding_size: Some(GlobalsUniform::min_size()),
        //             },
        //             count: None,
        //         },
        //     ],
        // });

        // We can create the sampler here since it won't change at runtime and doesn't depend on the view
        let sampler = render_device.create_sampler(&SamplerDescriptor::default());

        // Get the shader handle
        // let shader = world
        //     .resource::<AssetServer>()
        //     .load("shaders/video-glitch.wgsl");
        let shader = VIDEO_GLITCH_SHADER_HANDLE.clone();

        let pipeline_id = world
            .resource_mut::<PipelineCache>()
            // This will add the pipeline to the cache and queue it's creation
            .queue_render_pipeline(RenderPipelineDescriptor {
                label: Some("video_glitch_pipeline".into()),
                layout: vec![layout.clone()],
                // This will setup a fullscreen triangle for the vertex state
                vertex: fullscreen_shader_vertex_state(),
                fragment: Some(FragmentState {
                    shader,
                    shader_defs: vec![],
                    // Make sure this matches the entry point of your shader.
                    // It can be anything as long as it matches here and in the shader.
                    entry_point: "fragment".into(),
                    targets: vec![Some(ColorTargetState {
                        format: TextureFormat::bevy_default(),
                        blend: None,
                        write_mask: ColorWrites::ALL,
                    })],
                }),
                // All of the following properties are not important for this effect so just use the default values.
                // This struct doesn't have the Default trait implemented because not all field can have a default value.
                primitive: PrimitiveState::default(),
                depth_stencil: None,
                multisample: MultisampleState::default(),
                push_constant_ranges: vec![],
            });

        Self {
            layout,
            sampler,
            pipeline_id,
        }
    }
}

// This is the component that will get passed to the shader
#[derive(Component, Reflect, Clone, Copy, ExtractComponent, ShaderType)]
#[reflect(Component, Default)]
pub struct VideoGlitchSettings {
    /// Set the intensity of this glitch effect from [0, 1]. By default it has a
    /// value of 1.
    pub intensity: f32,
    /// This shader uses a color aberration matrix C in the following way: The
    /// first column `C[0] . color` selects the primary color, which is used to
    /// mix the other two. In practice this means one will not see the primary
    /// color in the color aberrations but will instead see traces of the
    /// secondary colors: `C[1] . color` and `C[2] . color`.
    ///
    /// The default value is an identity matrix, which specifies red as the
    /// primary color. Typically this matrix will be a doubly stochastic matrix
    /// meaning the columns and rows each sum to 1.
    pub color_aberration: Mat3,
    // WebGL2 structs must be 16 byte aligned.

    #[cfg(feature = "webgl2")]
    pub webgl2_padding: Vec2,
}

impl Default for VideoGlitchSettings {
    fn default() -> Self {
        Self {
            intensity: 1.0,
            color_aberration: Mat3::IDENTITY,
            #[cfg(feature = "webgl2")]
            webgl2_padding: Vec2::ZERO,
        }
    }
}
