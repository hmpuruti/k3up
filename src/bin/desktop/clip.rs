use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    event,
};

/// Draws its child inside a renderer layer bounded to the child's box. The software renderer
/// only clips text to the current layer, so unfocused inputs with long values would otherwise
/// paint past their edge.
pub struct Clip<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
}

pub fn clip<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Clip<'a, Message, Theme, Renderer> {
    Clip {
        content: content.into(),
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Clip<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget().layout(tree, renderer, limits)
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget()
            .operate(tree, layout, renderer, operation);
    }

    #[allow(clippy::too_many_arguments)]
    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        self.content.as_widget_mut().on_event(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        )
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Some(bounds) = layout.bounds().intersection(viewport) else {
            return;
        };
        renderer.with_layer(bounds, |renderer| {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, &bounds);
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, translation)
    }
}

impl<'a, Message, Theme, Renderer> From<Clip<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(widget: Clip<'a, Message, Theme, Renderer>) -> Self {
        Element::new(widget)
    }
}
